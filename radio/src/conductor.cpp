/**
 * conductor — see the header.
 */
#include "conductor.h"

#include "simclock.h"
#include "simradio.h"

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <dlfcn.h>
#include <pthread.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>
#include <vector>

namespace conductor {
namespace {

const struct simradio_services* B() { return simradio_services(); }

/* How long a grant may go unanswered, in wall time, before this station
 * reports idle anyway. A station whose tasks never all block — a spin, a wait
 * in a call nothing here can see — is then given time at the pace it actually
 * runs, rather than stopping the run. */
constexpr long kBusyGraceNs = 20 * 1000 * 1000;

/* How long, in wall time, an idle goes unanswered before it is said again
 * (resendIdle in the header). */
constexpr long kResendNs = 250 * 1000 * 1000;

std::atomic<int>     s_mode{-1};
std::atomic<int64_t> s_T{0};
std::atomic<int64_t> s_epoch{0};
std::atomic<bool>    s_joined{false};
std::atomic<int64_t> s_nodeAtJoin{0};
std::atomic<uint64_t> s_seq{0};
std::atomic<bool>    s_owed{false};
std::atomic<bool>    s_resending{false};   /* an idle is out and nothing has come back */
std::atomic<int64_t> s_until{kNever};
std::atomic<int64_t> s_lastUntil{kNever};
void               (*s_sendIdle)(uint64_t, int64_t) = nullptr;
std::atomic<void (*)(void)> s_onAdvance{nullptr};
int                  s_tfd = -1;

/* The real wall clock, past the time shim. */
int64_t rawWallUs()
{
    struct timespec ts;
    syscall(SYS_clock_gettime, CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000000 + ts.tv_nsec / 1000;
}

/* ---- f: the node's clock as a function of T ----
 *
 * SIMESH_CLOCK_PROFILE, when set, is "T:node,T:node,…" in microseconds, both
 * columns increasing: a monotone piecewise-linear map, slope 1 outside the
 * points. Absent, f is the identity. */
struct Point { int64_t t, n; };

const std::vector<Point>& profile()
{
    static const std::vector<Point> points = [] {
        std::vector<Point> out;
        const char* v = getenv("SIMESH_CLOCK_PROFILE");
        while (v && *v) {
            char* end = nullptr;
            long long t = strtoll(v, &end, 10);
            if (end == v || *end != ':') break;
            v = end + 1;
            long long n = strtoll(v, &end, 10);
            if (end == v) break;
            if (!out.empty() && (t <= out.back().t || n <= out.back().n)) break;
            out.push_back(Point{t, n});
            v = *end == ',' ? end + 1 : end;
            if (*end != ',') break;
        }
        return out;
    }();
    return points;
}

int64_t mapThrough(const std::vector<Point>& p, int64_t x, bool forward)
{
    auto from = [&](const Point& q) { return forward ? q.t : q.n; };
    auto to   = [&](const Point& q) { return forward ? q.n : q.t; };
    if (p.empty()) return x;
    if (x == kNever) return kNever;
    if (x <= from(p.front())) return to(p.front()) + (x - from(p.front()));
    for (size_t i = 1; i < p.size(); i++) {
        if (x <= from(p[i])) {
            int64_t dx = from(p[i]) - from(p[i - 1]);
            int64_t dy = to(p[i]) - to(p[i - 1]);
            return to(p[i - 1]) + (int64_t)((__int128)(x - from(p[i - 1])) * dy / dx);
        }
    }
    return to(p.back()) + (x - from(p.back()));
}

/* ---- The model's timers, in T ---- */

struct Timer {
    void      (*cb)(void*);
    void*       arg;
    void*       real;       /* the backend's, in a real-time run */
    int64_t     at;
    bool        armed;
};

struct Wake {
    void      (*cb)(void*);
    void*       arg;
    int64_t     atNode;
};

std::vector<Timer*>& timers() { static auto* v = new std::vector<Timer*>(); return *v; }
std::vector<Wake>&   wakes()  { static auto* v = new std::vector<Wake>();   return *v; }

/* Under the lock. */
int64_t computeUntil()
{
    int64_t until = kNever;
    for (Timer* t : timers())
        if (t->armed) until = std::min(until, t->at);
    for (const Wake& w : wakes())
        if (w.atNode != kNever) until = std::min(until, conductorOf(w.atNode));
    s_until.store(until);
    return until;
}

int64_t modelNow()
{
    return isVirtual() ? s_T.load() : B()->now_us();
}

void* timerCreate(void (*cb)(void*), void* arg, const char* name)
{
    Timer* t = new Timer{cb, arg, nullptr, 0, false};
    if (!isVirtual()) {
        t->real = B()->timer_create(cb, arg, name);
        return t;
    }
    B()->lock();
    timers().push_back(t);
    B()->unlock();
    return t;
}

void timerStartOnce(void* timer, int64_t delayUs)
{
    Timer* t = (Timer*)timer;
    if (!t) return;
    if (!isVirtual()) { B()->timer_start_once(t->real, delayUs); return; }
    B()->lock();
    t->at = s_T.load() + (delayUs < 0 ? 0 : delayUs);
    t->armed = true;
    computeUntil();
    B()->unlock();
}

void timerStop(void* timer)
{
    Timer* t = (Timer*)timer;
    if (!t) return;
    if (!isVirtual()) { B()->timer_stop(t->real); return; }
    B()->lock();
    t->armed = false;
    computeUntil();
    B()->unlock();
}

void lock()   { B()->lock(); }
void unlock() { B()->unlock(); }

int udpOpen(const char* bindAddr, const char* dest) { return B()->udp_open(bindAddr, dest); }

int spawnReader(int fd, void (*onDatagram)(const char*, size_t))
{
    return B()->spawn_reader(fd, onDatagram);
}

/* ---- The busy watchdog ---- */

void armWatchdog()
{
    if (s_tfd < 0) return;
    struct itimerspec its = {};
    its.it_value.tv_nsec = kBusyGraceNs;
    timerfd_settime(s_tfd, 0, &its, nullptr);
}

/* After an idle: the timer says it again kResendNs on unless the ether has
 * answered or the station has spoken first. */
void armResend()
{
    s_resending.store(true);
    if (s_tfd < 0) return;
    struct itimerspec its = {};
    its.it_value.tv_nsec = kResendNs;
    timerfd_settime(s_tfd, 0, &its, nullptr);
}

void sendIdleNow(int64_t until)
{
    s_lastUntil.store(until);
    if (s_sendIdle) s_sendIdle(s_seq.load(), until);
}

void* watchdogMain(void*)
{
    sigset_t all;
    sigfillset(&all);
    pthread_sigmask(SIG_BLOCK, &all, nullptr);
    for (;;) {
        uint64_t n;
        if (read(s_tfd, &n, sizeof n) != (ssize_t)sizeof n) continue;
        if (s_owed.exchange(false)) {
            sendIdleNow(s_until.load());
            armResend();
        } else if (s_resending.load()) {
            if (s_sendIdle) s_sendIdle(s_seq.load(), s_lastUntil.load());
            armResend();
        }
    }
    return nullptr;
}

void startWatchdog()
{
    s_tfd = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC);
    if (s_tfd < 0) {
        B()->log(SIMRADIO_LOG_WARN, "conductor: timerfd: %s", strerror(errno));
        return;
    }
    /* Created with every signal blocked, so that on a host whose tasks are
     * signal-driven this thread can never be where a signal lands. */
    sigset_t all, old;
    sigfillset(&all);
    pthread_sigmask(SIG_BLOCK, &all, &old);
    pthread_t th;
    int rc = pthread_create(&th, nullptr, watchdogMain, nullptr);
    pthread_sigmask(SIG_SETMASK, &old, nullptr);
    if (rc != 0) {
        B()->log(SIMRADIO_LOG_WARN, "conductor: no watchdog thread");
        close(s_tfd);
        s_tfd = -1;
        return;
    }
    pthread_detach(th);
}

/* ---- The time shim ---- */

int64_t opsNodeUs() { return nodeNowUs(); }
int64_t opsEpochUs() { return epochUs(); }
int     opsWakeCreate(void (*due)(void*), void* arg) { return wakeCreate(due, arg); }
void    opsWakeAt(int w, int64_t node) { wakeAt(w, node); }
void    opsIdle() { idle(); }

const struct simclock_ops kOps = { opsNodeUs, opsEpochUs, opsWakeCreate, opsWakeAt, opsIdle };

void attachShim()
{
    /* With every signal held off: dlsym takes the dynamic linker's lock, and a
     * host whose tasks are switched by signals must not switch this one out
     * while it holds it. */
    sigset_t all, old;
    sigfillset(&all);
    pthread_sigmask(SIG_BLOCK, &all, &old);
    auto attach = (simclock_attach_fn)dlsym(RTLD_DEFAULT, "simclock_attach");
    pthread_sigmask(SIG_SETMASK, &old, nullptr);
    if (attach) attach(&kOps);
    else B()->log(SIMRADIO_LOG_WARN, "conductor: virtual time without the time shim; "
                                     "the host's own clocks run in real time");
}

struct simradio_services s_model;

}  // namespace

bool isVirtual()
{
    int m = s_mode.load();
    if (m < 0) {
        const char* v = getenv("SIMESH_TIME");
        m = (v && strcmp(v, "virtual") == 0) ? 1 : 0;
        int expected = -1;
        if (!s_mode.compare_exchange_strong(expected, m)) m = expected;
        else if (m) {
            const char* e = getenv("SIMESH_EPOCH_US");
            s_epoch.store(e && *e ? strtoll(e, nullptr, 10) : rawWallUs());
        }
    }
    return m == 1;
}

const struct simradio_services* modelServices()
{
    static const struct simradio_services* made = [] {
        s_model = *B();
        s_model.now_us = modelNow;
        s_model.timer_create = timerCreate;
        s_model.timer_start_once = timerStartOnce;
        s_model.timer_stop = timerStop;
        s_model.lock = lock;
        s_model.unlock = unlock;
        s_model.udp_open = udpOpen;
        s_model.spawn_reader = spawnReader;
        return &s_model;
    }();
    return made;
}

int64_t nodeOf(int64_t t)       { return mapThrough(profile(), t, true); }
int64_t conductorOf(int64_t n)  { return mapThrough(profile(), n, false); }

int64_t nodeNowUs()
{
    return isVirtual() ? nodeOf(s_T.load()) : B()->now_us();
}

int64_t epochUs()
{
    if (isVirtual()) return s_epoch.load();
    static const int64_t e = rawWallUs() - B()->now_us();
    return e;
}

void advanceTo(int64_t t)
{
    if (!isVirtual()) return;
    B()->lock();
    bool moved = t > s_T.load();
    if (moved) s_T.store(t);
    B()->unlock();
    void (*hook)(void) = s_onAdvance.load();
    if (moved && hook) hook();
    B()->lock();
    for (;;) {
        int64_t now = s_T.load();
        int64_t best = kNever;
        Timer* timer = nullptr;
        int wake = -1;
        for (Timer* tm : timers())
            if (tm->armed && tm->at <= now && tm->at < best) { best = tm->at; timer = tm; }
        for (size_t i = 0; i < wakes().size(); i++) {
            Wake& w = wakes()[i];
            if (w.atNode == kNever) continue;
            int64_t at = conductorOf(w.atNode);
            if (at <= now && at < best) { best = at; timer = nullptr; wake = (int)i; }
        }
        void (*cb)(void*) = nullptr;
        void* arg = nullptr;
        if (timer) {
            timer->armed = false;
            cb = timer->cb;
            arg = timer->arg;
        } else if (wake >= 0) {
            wakes()[wake].atNode = kNever;
            cb = wakes()[wake].cb;
            arg = wakes()[wake].arg;
        } else {
            break;
        }
        computeUntil();
        B()->unlock();
        cb(arg);
        B()->lock();
    }
    computeUntil();
    B()->unlock();
}

void welcome(bool isVirtualRun, int64_t t, int64_t epoch, uint64_t seq)
{
    if (isVirtualRun != isVirtual()) {
        B()->log(SIMRADIO_LOG_ERROR, "conductor: the ether runs in %s time and this "
                 "station was started for %s", isVirtualRun ? "virtual" : "real",
                 isVirtual() ? "virtual" : "real");
        return;
    }
    if (!isVirtual()) return;
    if (epoch) s_epoch.store(epoch);
    advanceTo(t);
    s_nodeAtJoin.store(nodeOf(s_T.load()));
    s_joined.store(true);
    granted(seq);
}

int64_t nodeAtJoin() { return s_nodeAtJoin.load(); }
bool    joined()     { return s_joined.load(); }

void granted(uint64_t seq)
{
    if (!isVirtual()) return;
    s_seq.store(seq);
    s_resending.store(false);
    s_owed.store(true);
    armWatchdog();
}

void spoke()
{
    if (!isVirtual() || !s_joined.load()) return;
    s_resending.store(false);
    s_owed.store(true);
    armWatchdog();
}

uint64_t lastSeq() { return s_seq.load(); }

void resendIdle()
{
    if (!isVirtual() || !s_joined.load() || !s_sendIdle) return;
    s_sendIdle(s_seq.load(), s_lastUntil.load());
}

int wakeCreate(void (*due)(void*), void* arg)
{
    B()->lock();
    wakes().push_back(Wake{due, arg, kNever});
    int id = (int)wakes().size() - 1;
    B()->unlock();
    return id;
}

void wakeAt(int wake, int64_t nodeUs)
{
    B()->lock();
    if (wake >= 0 && wake < (int)wakes().size()) {
        wakes()[wake].atNode = nodeUs;
        computeUntil();
    }
    B()->unlock();
}

void idle()
{
    if (!isVirtual() || !s_joined.load()) return;
    B()->lock();
    int64_t until = computeUntil();
    B()->unlock();
    bool owed = s_owed.exchange(false);
    if (owed || until < s_lastUntil.load()) {
        sendIdleNow(until);
        armResend();
    }
}

void setIdleSender(void (*send)(uint64_t, int64_t))
{
    s_sendIdle = send;
}

void onAdvance(void (*moved)(void))
{
    s_onAdvance.store(moved);
}

void start()
{
    if (!isVirtual()) return;
    startWatchdog();
    attachShim();
}

}  // namespace conductor

extern "C" int simradio_virtual(void) { return conductor::isVirtual() ? 1 : 0; }
extern "C" int64_t simradio_node_us(void) { return conductor::nodeNowUs(); }
extern "C" int64_t simradio_epoch_us(void) { return conductor::epochUs(); }
extern "C" int64_t simradio_node_to_conductor(int64_t node) { return conductor::conductorOf(node); }
extern "C" int simradio_wake_create(void (*due)(void*), void* arg) { return conductor::wakeCreate(due, arg); }
extern "C" void simradio_wake_at(int wake, int64_t node_us) { conductor::wakeAt(wake, node_us); }
extern "C" void simradio_idle(void) { conductor::idle(); }
extern "C" void simradio_on_advance(void (*moved)(void)) { conductor::onAdvance(moved); }
