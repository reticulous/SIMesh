/**
 * simradio — an SX1262 on a virtual SPI bus, and the station's link to the
 * ether.
 *
 * A station links this library in place of a radio. Its driver hands each SPI
 * frame to `simradio_transfer` exactly as it would put it on a bus, and reads
 * the reply the datasheet describes; the model times every frame on the air,
 * raises the IRQ bits a real chip raises, and tells the ether what it is doing.
 * The ether (SIMesh/ether) decides who hears what and hands the frames that
 * reach this antenna back.
 *
 * Thread-safe throughout. Nothing here blocks the caller beyond the model's
 * own short critical section.
 */
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif
typedef struct simradio simradio_t;

enum { SIMRADIO_PIN_DIO1 = 1, SIMRADIO_PIN_BUSY = 2 };

/* The station's one link to the ether. Idempotent. An empty `ether_addr`
 * means "no ether": models exist, transmissions go nowhere. */
int simradio_station_open(int sid, const char* bind_addr, const char* ether_addr);

/* One chip per radio slot. `on_pin(ctx, pin, level)` runs on whatever
 * thread moved the line: the bus caller, the timer thread, or the reader. */
simradio_t* simradio_open(int slot, void (*on_pin)(void*, int, int), void* ctx);

/* One complete SPI frame, NSS low to NSS high. out[0] is the opcode; `in`
 * is the status byte until the data starts, then the data. Thread-safe. */
void simradio_transfer(simradio_t*, const uint8_t* out, size_t len, uint8_t* in);

/* The RST line's rising edge. */
void simradio_reset(simradio_t*);

/* What the model drives on a line right now. */
int simradio_pin(simradio_t*, int pin);

/* Microseconds on the model's clock, for a host that wants to log against it.
 * In a virtual-time run this is conductor time, T. */
int64_t simradio_now_us(void);

void simradio_close(simradio_t*);

/* ---- The station's clock ----
 *
 * A run keeps real or virtual time, for every station alike; the ether says
 * which in its welcome, and SIMESH_TIME=virtual in the environment says it
 * before the station can reach the ether. In virtual time the ether owns
 * conductor time T and moves it only when every station is idle. The host
 * reads node time, f(T) — its own crystal — and tells the library when it
 * next needs to run; the library tells the ether. */

/* 1 in a virtual-time run. */
int simradio_virtual(void);

/* Node time, µs: f(T) in a virtual run, the host's monotonic clock in a real one. */
int64_t simradio_node_us(void);

/* The wall-clock µs that node time 0 stands for. */
int64_t simradio_epoch_us(void);

/* f⁻¹: the conductor time at which node time reaches `node_us`. */
int64_t simradio_node_to_conductor(int64_t node_us);

/* A wake the host owns: `due(arg)` runs when a grant reaches the node time it
 * is set to, on the thread that received the grant, with no lock held. */
int  simradio_wake_create(void (*due)(void* arg), void* arg);
void simradio_wake_at(int wake, int64_t node_us);      /* INT64_MAX clears it */

/* The host has nothing to do before its wakes. The first call after each
 * grant tells the ether; later ones only when the next wake has moved closer. */
void simradio_idle(void);

/* A host whose own clock is a function of node time — a kernel tick counted
 * from it — learns of every move: `moved()` runs each time a grant moves T, on
 * the thread that received it, with no lock held, before any wake or timer
 * due at the new T. */
void simradio_on_advance(void (*moved)(void));
#ifdef __cplusplus
}
#endif
