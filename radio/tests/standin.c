/*
 * A stand-in station for the time shim's tests: the chip library, one thread
 * that sleeps in 25 ms steps, an interval timer at 10 ms, and a first thread
 * that blocks on a pipe. It prints one line per event, with node time:
 *
 *     sleeper <node us>        a 25 ms sleep ended
 *     alarm <count>            SIGALRM, counted in the handler
 *     clock <mono us> <wall s> what clock_gettime and time() said at start
 *     entropy <hex> <hex> <hex>
 *                              16 bytes of getentropy, then 8 of getrandom
 *                              and 8 of syscall(SYS_getrandom), drawn first
 *                              thing, before the chip library is opened
 *     tcp <text>               what came back over TCP for a console line,
 *                              given a second argument (relay, below)
 */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <pthread.h>
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
    }

    /* The alarm lands on the first thread only: the others block it. */
    pthread_sigmask(SIG_BLOCK, &alrm, NULL);
    pthread_t a, b;
    pthread_create(&a, NULL, sleeper, NULL);
    pthread_create(&b, NULL, reporter, NULL);
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
