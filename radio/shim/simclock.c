/**
 * libsimclock.so — the C library's time, answered in node time.
 *
 * Preloaded into a station started for a virtual-time run (SIMESH_TIME=
 * virtual). The station's chip library owns the clock; when it starts it finds
 * simclock_attach here and hands over what this needs (simclock.h). From then
 * on:
 *
 *   clock_gettime, gettimeofday, time        node time (+ the run's epoch for
 *                                            the wall clocks)
 *   nanosleep, clock_nanosleep, usleep, sleep
 *                                            until node time reaches the end, or
 *                                            a signal, as the real ones
 *   setitimer / getitimer (ITIMER_REAL)      SIGALRM once per interval of node
 *                                            time, on multiples of the interval
 *   poll, ppoll, select, pselect, epoll_wait, epoll_pwait
 *                                            the descriptors, or node time
 *                                            reaching the timeout
 *   pthread_cond_timedwait, pthread_cond_clockwait
 *                                            a signal, or node time reaching the
 *                                            deadline
 *
 * A wait blocks on a descriptor of its own thread's (an eventfd), which a wake
 * the chip library runs when a grant reaches the instant writes to. So a thread
 * waiting in node time is woken by the grant, and a signal still ends its wait
 * the way it ends a real one — which a host whose threads are switched by
 * signals depends on.
 *
 * With SIMESH_IDLE=threads the shim also keeps a census of the process's
 * threads: every thread created through pthread_create, and the first. A
 * thread is blocked while it is inside one of the waits above, an untimed
 * pthread_cond_wait, or a read, recv or accept on a blocking descriptor; when
 * the last one blocks, the station is idle, and the shim says so to the chip
 * library, whose wakes already hold every deadline the blocked threads are
 * waiting for. A wake that fires counts its thread as running at once, so the
 * station cannot look idle between the grant and the thread getting the CPU.
 *
 * Without SIMESH_TIME=virtual, and before the chip library attaches, every
 * call is the C library's own.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include "simclock.h"

#define NEVER INT64_MAX

static int s_virtual = -1;
static int s_census;
static int64_t s_epochEnv;
static const struct simclock_ops* _Atomic s_ops;

/* ---- The C library's own ---- */

static void* real_sym(void** slot, const char* name)
{
    void* p = *slot;
    if (!p) *slot = p = dlsym(RTLD_NEXT, name);
    return p;
}

#define REAL(name) ((__typeof__(r_##name))real_sym((void**)&r_##name, #name))

static int (*r_clock_gettime)(clockid_t, struct timespec*);
static int (*r_gettimeofday)(struct timeval*, void*);
static time_t (*r_time)(time_t*);
static int (*r_nanosleep)(const struct timespec*, struct timespec*);
static int (*r_clock_nanosleep)(clockid_t, int, const struct timespec*, struct timespec*);
static int (*r_usleep)(useconds_t);
static unsigned (*r_sleep)(unsigned);
static int (*r_setitimer)(__itimer_which_t, const struct itimerval*, struct itimerval*);
static int (*r_getitimer)(__itimer_which_t, struct itimerval*);
static int (*r_poll)(struct pollfd*, nfds_t, int);
static int (*r_ppoll)(struct pollfd*, nfds_t, const struct timespec*, const sigset_t*);
static int (*r_select)(int, fd_set*, fd_set*, fd_set*, struct timeval*);
static int (*r_pselect)(int, fd_set*, fd_set*, fd_set*, const struct timespec*, const sigset_t*);
static int (*r_epoll_wait)(int, struct epoll_event*, int, int);
static int (*r_epoll_pwait)(int, struct epoll_event*, int, int, const sigset_t*);
static int (*r_pthread_cond_timedwait)(pthread_cond_t*, pthread_mutex_t*, const struct timespec*);
static int (*r_pthread_cond_clockwait)(pthread_cond_t*, pthread_mutex_t*, clockid_t, const struct timespec*);
static int (*r_pthread_cond_wait)(pthread_cond_t*, pthread_mutex_t*);
static int (*r_pthread_create)(pthread_t*, const pthread_attr_t*, void* (*)(void*), void*);
static ssize_t (*r_read)(int, void*, size_t);
static ssize_t (*r_recv)(int, void*, size_t, int);
static ssize_t (*r_recvfrom)(int, void*, size_t, int, struct sockaddr*, socklen_t*);
static ssize_t (*r_recvmsg)(int, struct msghdr*, int);
static int (*r_accept)(int, struct sockaddr*, socklen_t*);
static int (*r_accept4)(int, struct sockaddr*, socklen_t*, int);

static void load_mode(void)
{
    if (s_virtual >= 0) return;
    const char* v = getenv("SIMESH_TIME");
    const char* i = getenv("SIMESH_IDLE");
    const char* e = getenv("SIMESH_EPOCH_US");
    s_census = i && strcmp(i, "threads") == 0;
    s_epochEnv = e && *e ? strtoll(e, NULL, 10) : 0;
    s_virtual = v && strcmp(v, "virtual") == 0;
}

/* The clock, when it is ours to answer; NULL means "the C library's". */
static const struct simclock_ops* ops(void)
{
    load_mode();
    if (!s_virtual) return NULL;
    return atomic_load(&s_ops);
}

static int64_t ts_us(const struct timespec* ts) { return (int64_t)ts->tv_sec * 1000000 + ts->tv_nsec / 1000; }
static void us_ts(int64_t us, struct timespec* ts) { ts->tv_sec = us / 1000000; ts->tv_nsec = (us % 1000000) * 1000; }
static int64_t tv_us(const struct timeval* tv) { return (int64_t)tv->tv_sec * 1000000 + tv->tv_usec; }
static void us_tv(int64_t us, struct timeval* tv) { tv->tv_sec = us / 1000000; tv->tv_usec = us % 1000000; }

static int monotonic_clock(clockid_t c)
{
    return c == CLOCK_MONOTONIC || c == CLOCK_MONOTONIC_RAW || c == CLOCK_MONOTONIC_COARSE ||
           c == CLOCK_BOOTTIME;
}

static int wall_clock(clockid_t c)
{
    return c == CLOCK_REALTIME || c == CLOCK_REALTIME_COARSE || c == CLOCK_TAI;
}

/* A wait's end, on the timer resolution: the next whole millisecond of node
 * time. Every instant a station asks for costs the whole run one barrier, and a
 * driver that sleeps ten microseconds at a time in a loop would otherwise ask
 * for a hundred of them per millisecond; aligned, the ends of many threads' and
 * many stations' waits also fall on the same instants. */
#define RESOLUTION_US 1000

static int64_t resolve(int64_t deadline)
{
    if (deadline == NEVER || deadline <= 0) return deadline;
    return ((deadline + RESOLUTION_US - 1) / RESOLUTION_US) * RESOLUTION_US;
}

/* Node time now, on a clock's own scale. Before the chip library attaches it
 * is 0: the station's time has not started, and a clock that read the host's
 * until then would run backwards when it did. */
static int64_t clock_now(const struct simclock_ops* o, clockid_t c)
{
    int64_t n = o ? o->node_us() : 0;
    int64_t epoch = o ? o->epoch_us() : s_epochEnv;
    return wall_clock(c) ? n + epoch : n;
}

/* ---- Threads: their wake, and the census ---- */

struct thread_rec {
    int         efd;        /* the eventfd a wake writes to */
    int         wake;       /* the chip library's wake for this thread */
    atomic_int  blocked;
    int         cond_wake;
    pthread_cond_t* _Atomic cond;   /* the condition a timed wait is on */
    int         counted;    /* in the census */
};

static __thread struct thread_rec* t_rec;
static atomic_int s_live = 1;       /* threads in the census; the first one to begin with */
static atomic_int s_blocked;
static pthread_mutex_t s_condLock = PTHREAD_MUTEX_INITIALIZER;
static pthread_key_t s_key;
static pthread_once_t s_keyOnce = PTHREAD_ONCE_INIT;

static void rec_gone(void* p)
{
    struct thread_rec* r = (struct thread_rec*)p;
    if (r && r->counted) {
        if (atomic_exchange(&r->blocked, 0)) atomic_fetch_sub(&s_blocked, 1);
        atomic_fetch_sub(&s_live, 1);
    }
}

static void make_key(void) { pthread_key_create(&s_key, rec_gone); }

static void wake_due(void* arg)
{
    struct thread_rec* r = (struct thread_rec*)arg;
    if (atomic_exchange(&r->blocked, 0)) atomic_fetch_sub(&s_blocked, 1);
    uint64_t one = 1;
    ssize_t w = write(r->efd, &one, sizeof one);
    (void)w;
}

static void cond_due(void* arg)
{
    struct thread_rec* r = (struct thread_rec*)arg;
    if (atomic_exchange(&r->blocked, 0)) atomic_fetch_sub(&s_blocked, 1);
    sigset_t all, old;
    sigfillset(&all);
    pthread_sigmask(SIG_BLOCK, &all, &old);
    pthread_mutex_lock(&s_condLock);
    pthread_cond_t* c = atomic_load(&r->cond);
    if (c) pthread_cond_broadcast(c);
    pthread_mutex_unlock(&s_condLock);
    pthread_sigmask(SIG_SETMASK, &old, NULL);
}

static struct thread_rec* rec(const struct simclock_ops* o)
{
    if (t_rec) return t_rec;
    struct thread_rec* r = (struct thread_rec*)calloc(1, sizeof *r);
    if (!r) return NULL;
    r->efd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    r->wake = o->wake_create(wake_due, r);
    r->cond_wake = o->wake_create(cond_due, r);
    /* The process's first thread is in the census from the start; every other
     * one arrives through pthread_create. */
    r->counted = gettid() == getpid();
    pthread_once(&s_keyOnce, make_key);
    pthread_setspecific(s_key, r);
    t_rec = r;
    return r;
}

/* The census: the station is idle when every thread it counts is blocked. */
static void census_block(const struct simclock_ops* o, struct thread_rec* r)
{
    if (!s_census || !o || !r || !r->counted) return;
    if (!atomic_exchange(&r->blocked, 1)) {
        int b = atomic_fetch_add(&s_blocked, 1) + 1;
        if (b >= atomic_load(&s_live)) o->idle();
    }
}

static void census_run(struct thread_rec* r)
{
    if (!s_census || !r || !r->counted) return;
    if (atomic_exchange(&r->blocked, 0)) atomic_fetch_sub(&s_blocked, 1);
}

static void drain(int efd)
{
    uint64_t n;
    while (REAL(read)(efd, &n, sizeof n) == (ssize_t)sizeof n) {}
}

struct start {
    void* (*fn)(void*);
    void* arg;
};

static void* trampoline(void* p)
{
    struct start s = *(struct start*)p;
    free(p);
    pthread_once(&s_keyOnce, make_key);
    struct thread_rec* r = (struct thread_rec*)calloc(1, sizeof *r);
    if (r) {
        r->efd = -1;
        r->wake = -1;
        r->cond_wake = -1;
        r->counted = 1;
        pthread_setspecific(s_key, r);
        t_rec = r;
    }
    return s.fn(s.arg);
}

int pthread_create(pthread_t* th, const pthread_attr_t* attr, void* (*fn)(void*), void* arg)
{
    load_mode();
    if (!s_virtual || !s_census) return REAL(pthread_create)(th, attr, fn, arg);
    struct start* s = (struct start*)malloc(sizeof *s);
    if (!s) return EAGAIN;
    s->fn = fn;
    s->arg = arg;
    atomic_fetch_add(&s_live, 1);
    int rc = REAL(pthread_create)(th, attr, trampoline, s);
    if (rc != 0) {
        atomic_fetch_sub(&s_live, 1);
        free(s);
    }
    return rc;
}

/* A thread made by trampoline has a record without a wake; give it one. */
static struct thread_rec* ready_rec(const struct simclock_ops* o)
{
    struct thread_rec* r = t_rec;
    if (!r) return rec(o);
    if (r->efd < 0) {
        r->efd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
        r->wake = o->wake_create(wake_due, r);
        r->cond_wake = o->wake_create(cond_due, r);
    }
    return r;
}

/* A wait with no timeout has nothing to do with the clock: the C library's
 * own, counted in the census as a blocked thread. */
#define UNTIMED_CALL(call)                                            \
    do {                                                              \
        if (!s_census) return call;                                   \
        struct thread_rec* r_ = ready_rec(o);                         \
        census_block(o, r_);                                          \
        __typeof__(call) rc_ = call;                                  \
        int e_ = errno;                                               \
        census_run(r_);                                               \
        errno = e_;                                                   \
        return rc_;                                                   \
    } while (0)

/* Wait on this thread's eventfd, and `extra` descriptors, until one is ready,
 * a signal arrives, or node time reaches `deadline` (µs, NEVER for none).
 * Returns what the real ppoll returned over the extra descriptors (-1 EINTR
 * on a signal), or 0 when the deadline came first. */
static int node_wait(const struct simclock_ops* o, struct pollfd* extra, nfds_t n,
                     int64_t deadline, const sigset_t* mask)
{
    struct thread_rec* r = ready_rec(o);
    if (!r || r->efd < 0) return -2;
    deadline = resolve(deadline);
    struct pollfd local[64];
    struct pollfd* fds = n + 1 <= 64 ? local : (struct pollfd*)malloc((n + 1) * sizeof *fds);
    if (!fds) return -2;
    if (n) memcpy(fds, extra, n * sizeof *fds);
    fds[n].fd = r->efd;
    fds[n].events = POLLIN;
    if (deadline != NEVER) o->wake_at(r->wake, deadline);
    int result = 0;
    for (;;) {
        if (deadline != NEVER && o->node_us() >= deadline) { result = 0; break; }
        census_block(o, r);
        fds[n].revents = 0;
        int rc = REAL(ppoll)(fds, n + 1, NULL, mask);
        census_run(r);
        if (rc < 0) { result = -1; break; }
        if (fds[n].revents) drain(r->efd);
        int ready = 0;
        for (nfds_t i = 0; i < n; i++) if (fds[i].revents) ready++;
        if (ready) {
            memcpy(extra, fds, n * sizeof *fds);
            result = ready;
            break;
        }
    }
    int saved = errno;
    if (deadline != NEVER) o->wake_at(r->wake, NEVER);
    if (fds != local) free(fds);
    errno = saved;
    return result;
}

/* ---- Clocks ---- */

int clock_gettime(clockid_t c, struct timespec* ts)
{
    load_mode();
    if (!s_virtual || !(monotonic_clock(c) || wall_clock(c))) return REAL(clock_gettime)(c, ts);
    us_ts(clock_now(ops(), c), ts);
    return 0;
}

int gettimeofday(struct timeval* tv, void* tz)
{
    load_mode();
    if (!s_virtual) return REAL(gettimeofday)(tv, tz);
    us_tv(clock_now(ops(), CLOCK_REALTIME), tv);
    return 0;
}

time_t time(time_t* out)
{
    load_mode();
    if (!s_virtual) return REAL(time)(out);
    time_t t = (time_t)(clock_now(ops(), CLOCK_REALTIME) / 1000000);
    if (out) *out = t;
    return t;
}

/* ---- Sleeps ---- */

static int sleep_until(const struct simclock_ops* o, int64_t deadline, int64_t* left)
{
    int rc = node_wait(o, NULL, 0, deadline, NULL);
    if (rc == -1) {
        if (left) {
            int64_t l = deadline - o->node_us();
            *left = l > 0 ? l : 0;
        }
        errno = EINTR;
        return -1;
    }
    return 0;
}

int nanosleep(const struct timespec* req, struct timespec* rem)
{
    const struct simclock_ops* o = ops();
    if (!o || !req) return REAL(nanosleep)(req, rem);
    int64_t left = 0;
    int rc = sleep_until(o, o->node_us() + ts_us(req), &left);
    if (rc < 0 && rem) us_ts(left, rem);
    return rc;
}

int clock_nanosleep(clockid_t c, int flags, const struct timespec* req, struct timespec* rem)
{
    const struct simclock_ops* o = ops();
    if (!o || !req || !(monotonic_clock(c) || wall_clock(c)))
        return REAL(clock_nanosleep)(c, flags, req, rem);
    int64_t deadline = (flags & TIMER_ABSTIME)
        ? ts_us(req) - (wall_clock(c) ? o->epoch_us() : 0)
        : o->node_us() + ts_us(req);
    int64_t left = 0;
    if (sleep_until(o, deadline, &left) < 0) {
        if (rem && !(flags & TIMER_ABSTIME)) us_ts(left, rem);
        return EINTR;
    }
    return 0;
}

int usleep(useconds_t us)
{
    const struct simclock_ops* o = ops();
    if (!o) return REAL(usleep)(us);
    return sleep_until(o, o->node_us() + us, NULL);
}

unsigned sleep(unsigned s)
{
    const struct simclock_ops* o = ops();
    if (!o) return REAL(sleep)(s);
    int64_t left = 0;
    if (sleep_until(o, o->node_us() + (int64_t)s * 1000000, &left) < 0)
        return (unsigned)((left + 999999) / 1000000);
    return 0;
}

/* ---- The interval timer ---- */

static struct itimerval s_itimer;
static int64_t s_itNext = NEVER;
static int s_itWake = -1;

static int64_t it_first(int64_t now, int64_t value, int64_t interval)
{
    int64_t at = now + value;
    if (interval > 0) at = ((at + interval - 1) / interval) * interval;   /* on its multiples */
    return at;
}

static void it_due(void* arg)
{
    (void)arg;
    const struct simclock_ops* o = atomic_load(&s_ops);
    int64_t interval = tv_us(&s_itimer.it_interval);
    if (interval > 0) {
        int64_t now = o->node_us();
        int64_t next = s_itNext + interval;
        while (next <= now) next += interval;
        s_itNext = next;
        o->wake_at(s_itWake, next);
    } else {
        s_itNext = NEVER;
    }
    kill(getpid(), SIGALRM);
}

static void it_arm(const struct simclock_ops* o)
{
    if (s_itWake < 0) s_itWake = o->wake_create(it_due, NULL);
    int64_t value = tv_us(&s_itimer.it_value);
    s_itNext = value > 0 ? it_first(o->node_us(), value, tv_us(&s_itimer.it_interval)) : NEVER;
    o->wake_at(s_itWake, s_itNext);
}

int setitimer(__itimer_which_t which, const struct itimerval* nv, struct itimerval* old)
{
    load_mode();
    if (!s_virtual || which != ITIMER_REAL) return REAL(setitimer)(which, nv, old);
    if (old) *old = s_itimer;
    if (nv) s_itimer = *nv;
    const struct simclock_ops* o = atomic_load(&s_ops);
    if (o) it_arm(o);
    return 0;
}

int getitimer(__itimer_which_t which, struct itimerval* cur)
{
    load_mode();
    if (!s_virtual || which != ITIMER_REAL) return REAL(getitimer)(which, cur);
    if (cur) {
        *cur = s_itimer;
        const struct simclock_ops* o = atomic_load(&s_ops);
        if (o && s_itNext != NEVER) {
            int64_t left = s_itNext - o->node_us();
            us_tv(left > 0 ? left : 0, &cur->it_value);
        }
    }
    return 0;
}

/* ---- Descriptor waits with a timeout ---- */

int ppoll(struct pollfd* fds, nfds_t n, const struct timespec* tmo, const sigset_t* mask)
{
    const struct simclock_ops* o = ops();
    if (!o || (tmo && tmo->tv_sec == 0 && tmo->tv_nsec == 0))
        return REAL(ppoll)(fds, n, tmo, mask);
    if (!tmo) UNTIMED_CALL(REAL(ppoll)(fds, n, tmo, mask));
    int64_t deadline = o->node_us() + ts_us(tmo);
    int rc = node_wait(o, fds, n, deadline, mask);
    if (rc == -2) return REAL(ppoll)(fds, n, tmo, mask);
    if (rc == 0) for (nfds_t i = 0; i < n; i++) fds[i].revents = 0;
    return rc;
}

int poll(struct pollfd* fds, nfds_t n, int timeout)
{
    const struct simclock_ops* o = ops();
    if (!o || timeout == 0) return REAL(poll)(fds, n, timeout);
    struct timespec ts;
    us_ts((int64_t)timeout * 1000, &ts);
    return ppoll(fds, n, timeout < 0 ? NULL : &ts, NULL);
}

static int sets_to_poll(int nfds, fd_set* r, fd_set* w, fd_set* e, struct pollfd* out, int cap)
{
    int n = 0;
    for (int fd = 0; fd < nfds && n < cap; fd++) {
        short ev = 0;
        if (r && FD_ISSET(fd, r)) ev |= POLLIN;
        if (w && FD_ISSET(fd, w)) ev |= POLLOUT;
        if (e && FD_ISSET(fd, e)) ev |= POLLPRI;
        if (!ev) continue;
        out[n].fd = fd;
        out[n].events = ev;
        out[n].revents = 0;
        n++;
    }
    return n;
}

static int pselect_node(int nfds, fd_set* r, fd_set* w, fd_set* e, int64_t deadline,
                        const sigset_t* mask, const struct simclock_ops* o)
{
    struct pollfd fds[FD_SETSIZE];
    int n = sets_to_poll(nfds, r, w, e, fds, FD_SETSIZE);
    int rc = node_wait(o, fds, (nfds_t)n, deadline, mask);
    if (rc < 0) return rc;
    if (r) FD_ZERO(r);
    if (w) FD_ZERO(w);
    if (e) FD_ZERO(e);
    int count = 0;
    for (int i = 0; i < n && rc > 0; i++) {
        short re = fds[i].revents;
        if (r && (re & (POLLIN | POLLHUP | POLLERR))) { FD_SET(fds[i].fd, r); count++; }
        if (w && (re & (POLLOUT | POLLERR))) { FD_SET(fds[i].fd, w); count++; }
        if (e && (re & POLLPRI)) { FD_SET(fds[i].fd, e); count++; }
    }
    return count;
}

int pselect(int nfds, fd_set* r, fd_set* w, fd_set* e, const struct timespec* tmo,
            const sigset_t* mask)
{
    const struct simclock_ops* o = ops();
    if (!o || (tmo && tmo->tv_sec == 0 && tmo->tv_nsec == 0))
        return REAL(pselect)(nfds, r, w, e, tmo, mask);
    if (!tmo) UNTIMED_CALL(REAL(pselect)(nfds, r, w, e, tmo, mask));
    int rc = pselect_node(nfds, r, w, e, o->node_us() + ts_us(tmo), mask, o);
    if (rc == -2) return REAL(pselect)(nfds, r, w, e, tmo, mask);
    return rc;
}

int select(int nfds, fd_set* r, fd_set* w, fd_set* e, struct timeval* tmo)
{
    const struct simclock_ops* o = ops();
    if (!o || (tmo && tmo->tv_sec == 0 && tmo->tv_usec == 0))
        return REAL(select)(nfds, r, w, e, tmo);
    if (!tmo) UNTIMED_CALL(REAL(select)(nfds, r, w, e, tmo));
    int rc = pselect_node(nfds, r, w, e, o->node_us() + tv_us(tmo), NULL, o);
    if (rc == -2) return REAL(select)(nfds, r, w, e, tmo);
    return rc;
}

int epoll_pwait(int epfd, struct epoll_event* ev, int max, int timeout, const sigset_t* mask)
{
    const struct simclock_ops* o = ops();
    if (!o || timeout == 0) return REAL(epoll_pwait)(epfd, ev, max, timeout, mask);
    if (timeout < 0) UNTIMED_CALL(REAL(epoll_pwait)(epfd, ev, max, timeout, mask));
    int64_t deadline = o->node_us() + (int64_t)timeout * 1000;
    for (;;) {
        struct pollfd p = { epfd, POLLIN, 0 };
        int rc = node_wait(o, &p, 1, deadline, mask);
        if (rc == -2) return REAL(epoll_pwait)(epfd, ev, max, timeout, mask);
        if (rc <= 0) return rc;
        rc = REAL(epoll_wait)(epfd, ev, max, 0);
        if (rc != 0) return rc;
    }
}

int epoll_wait(int epfd, struct epoll_event* ev, int max, int timeout)
{
    return epoll_pwait(epfd, ev, max, timeout, NULL);
}

/* ---- Condition variables ---- */

static int cond_node_wait(const struct simclock_ops* o, pthread_cond_t* c, pthread_mutex_t* m,
                          int64_t deadline)
{
    struct thread_rec* r = ready_rec(o);
    if (!r || r->cond_wake < 0) return -2;
    if (o->node_us() >= deadline) return ETIMEDOUT;
    sigset_t all, old;
    sigfillset(&all);
    pthread_sigmask(SIG_BLOCK, &all, &old);
    pthread_mutex_lock(&s_condLock);
    atomic_store(&r->cond, c);
    pthread_mutex_unlock(&s_condLock);
    pthread_sigmask(SIG_SETMASK, &old, NULL);
    deadline = resolve(deadline);
    o->wake_at(r->cond_wake, deadline);
    census_block(o, r);
    int rc = REAL(pthread_cond_wait)(c, m);
    census_run(r);
    o->wake_at(r->cond_wake, NEVER);
    pthread_sigmask(SIG_BLOCK, &all, &old);
    pthread_mutex_lock(&s_condLock);
    atomic_store(&r->cond, NULL);
    pthread_mutex_unlock(&s_condLock);
    pthread_sigmask(SIG_SETMASK, &old, NULL);
    return rc;
}

int pthread_cond_timedwait(pthread_cond_t* c, pthread_mutex_t* m, const struct timespec* abst)
{
    const struct simclock_ops* o = ops();
    if (!o) return REAL(pthread_cond_timedwait)(c, m, abst);
    int rc = cond_node_wait(o, c, m, ts_us(abst) - o->epoch_us());
    return rc == -2 ? REAL(pthread_cond_timedwait)(c, m, abst) : rc;
}

int pthread_cond_clockwait(pthread_cond_t* c, pthread_mutex_t* m, clockid_t clk,
                           const struct timespec* abst)
{
    const struct simclock_ops* o = ops();
    if (!o || !(monotonic_clock(clk) || wall_clock(clk)))
        return REAL(pthread_cond_clockwait)(c, m, clk, abst);
    int rc = cond_node_wait(o, c, m, ts_us(abst) - (wall_clock(clk) ? o->epoch_us() : 0));
    return rc == -2 ? REAL(pthread_cond_clockwait)(c, m, clk, abst) : rc;
}

int pthread_cond_wait(pthread_cond_t* c, pthread_mutex_t* m)
{
    const struct simclock_ops* o = ops();
    if (!o || !s_census) return REAL(pthread_cond_wait)(c, m);
    struct thread_rec* r = ready_rec(o);
    census_block(o, r);
    int rc = REAL(pthread_cond_wait)(c, m);
    census_run(r);
    return rc;
}

/* ---- Blocking reads, for the census ---- */

static int blocking(int fd, int flags)
{
    if (flags & MSG_DONTWAIT) return 0;
    int fl = fcntl(fd, F_GETFL);
    return fl >= 0 && !(fl & O_NONBLOCK);
}

#define CENSUS_CALL(call, fd, flags)                                  \
    do {                                                              \
        const struct simclock_ops* o_ = ops();                        \
        if (!o_ || !s_census || !blocking(fd, flags)) return call;    \
        struct thread_rec* r_ = ready_rec(o_);                        \
        census_block(o_, r_);                                         \
        __typeof__(call) rc_ = call;                                  \
        int e_ = errno;                                               \
        census_run(r_);                                               \
        errno = e_;                                                   \
        return rc_;                                                   \
    } while (0)

ssize_t read(int fd, void* buf, size_t n) { CENSUS_CALL(REAL(read)(fd, buf, n), fd, 0); }
ssize_t recv(int fd, void* buf, size_t n, int fl) { CENSUS_CALL(REAL(recv)(fd, buf, n, fl), fd, fl); }
ssize_t recvfrom(int fd, void* buf, size_t n, int fl, struct sockaddr* a, socklen_t* al)
{
    CENSUS_CALL(REAL(recvfrom)(fd, buf, n, fl, a, al), fd, fl);
}
ssize_t recvmsg(int fd, struct msghdr* m, int fl) { CENSUS_CALL(REAL(recvmsg)(fd, m, fl), fd, fl); }
int accept(int fd, struct sockaddr* a, socklen_t* al) { CENSUS_CALL(REAL(accept)(fd, a, al), fd, 0); }
int accept4(int fd, struct sockaddr* a, socklen_t* al, int fl) { CENSUS_CALL(REAL(accept4)(fd, a, al, fl), fd, 0); }

/* ---- The chip library arrives ---- */

void simclock_attach(const struct simclock_ops* o)
{
    load_mode();
    if (!s_virtual) return;
    atomic_store(&s_ops, o);
    if (tv_us(&s_itimer.it_value) > 0) it_arm(o);
}

/* Every C library function is looked up here, before main(), and never later:
 * dlsym takes the dynamic linker's lock, and on a host whose tasks are
 * switched by signals a task can be switched out holding it, leaving the next
 * task that looks something up blocked on it for good. */
__attribute__((constructor)) static void simclock_init(void)
{
    load_mode();
    (void)REAL(clock_gettime); (void)REAL(gettimeofday); (void)REAL(time);
    (void)REAL(nanosleep); (void)REAL(clock_nanosleep); (void)REAL(usleep);
    (void)REAL(sleep); (void)REAL(setitimer); (void)REAL(getitimer);
    (void)REAL(poll); (void)REAL(ppoll); (void)REAL(select); (void)REAL(pselect);
    (void)REAL(epoll_wait); (void)REAL(epoll_pwait);
    (void)REAL(pthread_cond_timedwait); (void)REAL(pthread_cond_clockwait);
    (void)REAL(pthread_cond_wait); (void)REAL(pthread_create);
    (void)REAL(read); (void)REAL(recv); (void)REAL(recvfrom); (void)REAL(recvmsg);
    (void)REAL(accept); (void)REAL(accept4);
}
