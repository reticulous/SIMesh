/*
 * A stand-in station for the time shim's tests: the chip library, one thread
 * that sleeps in 25 ms steps, an interval timer at 10 ms, and a first thread
 * that blocks on a pipe. It prints one line per event, with node time:
 *
 *     sleeper <node us>        a 25 ms sleep ended
 *     waiter <node us> <rc>    a 40 ms pthread_cond_timedwait nobody signals
 *                              ended, and what it returned
 *     monowaiter <node us> <rc>
 *                              the same, 30 ms, on a condition that keeps the
 *                              monotonic clock
 *     semwaiter <node us> <errno>
 *                              a 30 ms sem_clockwait nobody posts ended, and
 *                              the errno it left
 *     posted <node us>         a sem_wait ended by the semwaiter's post,
 *                              every fourth of its waits
 *     alarm <count>            SIGALRM, counted in the handler
 *     clock <mono us> <wall s> what clock_gettime and time() said at start
 *     entropy <hex> <hex> <hex>
 *                              16 bytes of getentropy, then 8 of getrandom
 *                              and 8 of syscall(SYS_getrandom), drawn first
 *                              thing, before the chip library is opened
 *     tcp <text>               what came back over TCP for a console line,
 *                              given a second argument (relay, below)
 *     listening <port>         the port it listens on at SIMESH_BIND_ADDR,
 *                              given a second argument
 */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <pthread.h>
#include <semaphore.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/random.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include "simradio.h"

static volatile sig_atomic_t alarms;
static int alarm_pipe[2];

static void on_alarm(int sig)
{
    (void)sig;
    alarms++;
    char c = 'a';
    ssize_t w = write(alarm_pipe[1], &c, 1);
    (void)w;
}

static int64_t mono_us(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000 + ts.tv_nsec / 1000;
}

static void* sleeper(void* arg)
{
    (void)arg;
    for (;;) {
        struct timespec req = { 0, 25 * 1000 * 1000 }, rem;
        while (nanosleep(&req, &rem) != 0 && errno == EINTR) req = rem;
        printf("sleeper %lld\n", (long long)mono_us());
        fflush(stdout);
    }
    return NULL;
}

static void* waiter(void* arg)
{
    (void)arg;
    pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
    pthread_cond_t c = PTHREAD_COND_INITIALIZER;
    pthread_mutex_lock(&m);
    for (;;) {
        struct timespec end;
        clock_gettime(CLOCK_REALTIME, &end);
        end.tv_nsec += 40 * 1000 * 1000;
        if (end.tv_nsec >= 1000000000) {
            end.tv_sec += 1;
            end.tv_nsec -= 1000000000;
        }
        int rc = pthread_cond_timedwait(&c, &m, &end);
        printf("waiter %lld %d\n", (long long)mono_us(), rc);
        fflush(stdout);
    }
    return NULL;
}

static void add_ms(struct timespec* ts, long ms)
{
    ts->tv_nsec += ms * 1000 * 1000;
    if (ts->tv_nsec >= 1000000000) {
        ts->tv_sec += 1;
        ts->tv_nsec -= 1000000000;
    }
}

static void* monowaiter(void* arg)
{
    (void)arg;
    pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
    pthread_condattr_t ca;
    pthread_condattr_init(&ca);
    pthread_condattr_setclock(&ca, CLOCK_MONOTONIC);
    pthread_cond_t c;
    pthread_cond_init(&c, &ca);
    pthread_mutex_lock(&m);
    for (;;) {
        struct timespec end;
        clock_gettime(CLOCK_MONOTONIC, &end);
        add_ms(&end, 30);
        int rc = pthread_cond_timedwait(&c, &m, &end);
        printf("monowaiter %lld %d\n", (long long)mono_us(), rc);
        fflush(stdout);
    }
    return NULL;
}

static sem_t s_never, s_posted;

static void* posted(void* arg)
{
    (void)arg;
    for (;;) {
        if (sem_wait(&s_posted) != 0) continue;
        printf("posted %lld\n", (long long)mono_us());
        fflush(stdout);
    }
    return NULL;
}

static void* semwaiter(void* arg)
{
    (void)arg;
    for (int n = 1;; n++) {
        struct timespec end;
        clock_gettime(CLOCK_MONOTONIC, &end);
        add_ms(&end, 30);
        int rc = sem_clockwait(&s_never, CLOCK_MONOTONIC, &end);
        printf("semwaiter %lld %d\n", (long long)mono_us(), rc == 0 ? 0 : errno);
        fflush(stdout);
        if (n % 4 == 0) sem_post(&s_posted);
    }
    return NULL;
}

static void* reporter(void* arg)
{
    (void)arg;
    char c;
    while (read(alarm_pipe[0], &c, 1) == 1) {
        printf("alarm %d %lld\n", (int)alarms, (long long)mono_us());
        fflush(stdout);
    }
    return NULL;
}

/* With a second argument, host:port: connect there over TCP, send each line
 * read from the console a byte at a time, and print what comes back. */
static void* relay(void* arg)
{
    char host[64];
    const char* colon = strrchr((const char*)arg, ':');
    if (!colon || (size_t)(colon - (const char*)arg) >= sizeof host) return NULL;
    memcpy(host, arg, (size_t)(colon - (const char*)arg));
    host[colon - (const char*)arg] = '\0';
    struct sockaddr_in to = { 0 };
    to.sin_family = AF_INET;
    to.sin_port = htons((uint16_t)atoi(colon + 1));
    inet_pton(AF_INET, host, &to.sin_addr);
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (connect(fd, (struct sockaddr*)&to, sizeof to) != 0) return NULL;
    char line[256];
    size_t len = 0;
    char c;
    while (read(0, &c, 1) == 1) {
        if (len < sizeof line) line[len++] = c;
        if (c != '\n') continue;
        if (write(fd, line, len) != (ssize_t)len) break;
        len = 0;
        char back[256];
        ssize_t n = recv(fd, back, sizeof back - 1, 0);
        if (n <= 0) break;
        back[n] = '\0';
        printf("tcp %s", back);
        fflush(stdout);
    }
    return NULL;
}

static void print_hex(const unsigned char* b, size_t n)
{
    printf(" ");
    for (size_t i = 0; i < n; i++) printf("%02x", b[i]);
}

int main(int argc, char** argv)
{
    if (argc < 2) return 2;
    unsigned char e[16], r[8], s[8];
    if (getentropy(e, sizeof e) != 0) return 1;
    if (getrandom(r, sizeof r, 0) != (ssize_t)sizeof r) return 1;
    if (syscall(SYS_getrandom, s, sizeof s, 0) != (long)sizeof s) return 1;
    printf("entropy");
    print_hex(e, sizeof e);
    print_hex(r, sizeof r);
    print_hex(s, sizeof s);
    printf("\n");
    fflush(stdout);

    if (pipe(alarm_pipe) != 0) return 1;
    sigset_t alrm;
    sigemptyset(&alrm);
    sigaddset(&alrm, SIGALRM);
    struct sigaction sa = { 0 };
    sa.sa_handler = on_alarm;
    sigaction(SIGALRM, &sa, NULL);

    if (simradio_station_open(3, "127.0.0.1", argv[1]) != 0) return 1;
    printf("clock %lld %lld\n", (long long)mono_us(), (long long)time(NULL));
    fflush(stdout);

    if (argc > 2) {
        pthread_t r;
        pthread_create(&r, NULL, relay, argv[2]);
        const char* own = getenv("SIMESH_BIND_ADDR");
        int l = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in at = { 0 };
        at.sin_family = AF_INET;
        inet_pton(AF_INET, own ? own : "127.0.0.1", &at.sin_addr);
        socklen_t al = sizeof at;
        if (bind(l, (struct sockaddr*)&at, sizeof at) != 0 || listen(l, 1) != 0
            || getsockname(l, (struct sockaddr*)&at, &al) != 0)
            return 1;
        printf("listening %u\n", ntohs(at.sin_port));
        fflush(stdout);
    }

    sem_init(&s_never, 0, 0);
    sem_init(&s_posted, 0, 0);

    /* The alarm lands on the first thread only: the others block it. */
    pthread_sigmask(SIG_BLOCK, &alrm, NULL);
    pthread_t a, b, w, mw, sw, p;
    pthread_create(&a, NULL, sleeper, NULL);
    pthread_create(&b, NULL, reporter, NULL);
    pthread_create(&w, NULL, waiter, NULL);
    pthread_create(&mw, NULL, monowaiter, NULL);
    pthread_create(&sw, NULL, semwaiter, NULL);
    pthread_create(&p, NULL, posted, NULL);
    pthread_sigmask(SIG_UNBLOCK, &alrm, NULL);

    struct itimerval it = { { 0, 10000 }, { 0, 10000 } };
    setitimer(ITIMER_REAL, &it, NULL);

    int never[2];
    if (pipe(never) != 0) return 1;
    char c;
    for (;;) {
        ssize_t n = read(never[0], &c, 1);
        if (n < 0 && errno == EINTR) continue;
        break;
    }
    return 0;
}
