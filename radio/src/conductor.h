/**
 * conductor — the station's side of the ether's clock.
 *
 * A run keeps real time or virtual time, for every station alike. In real
 * time the model's clock and timers are the backend's. In virtual time the
 * ether owns conductor time T and advances it only when every station has
 * said it is idle; this file keeps the last T the ether granted, runs the
 * model's timers and the host's wakes when a grant reaches their instant, and
 * tells the ether when this station next needs to run.
 *
 * The model reads T. The host reads node time, f(T): its own crystal, which a
 * per-node profile may bend; nodeOf and conductorOf are that map and its
 * inverse, and the only place it is defined.
 */
#pragma once

#include <stddef.h>
#include <stdint.h>

#include "services.h"

namespace conductor {

constexpr int64_t kNever = INT64_MAX;

/** True in a virtual-time run. Read from SIMESH_TIME at the first call, and
 *  confirmed by the ether's welcome. */
bool isVirtual();

/** The services the model and the link use: the backend's, except that the
 *  clock and the one-shot timers are T's in a virtual run. */
const struct simradio_services* modelServices();

/** f and f⁻¹. */
int64_t nodeOf(int64_t t);
int64_t conductorOf(int64_t node);

int64_t nodeNowUs();
int64_t epochUs();

/** Node time when the ether's welcome arrived, and whether it has. A host
 *  whose clock counts from its own boot counts from here. */
int64_t nodeAtJoin();
bool    joined();

/* ---- What the ether's messages do ---- */

/** A timed message from the ether: T moves to `t`, and whatever falls due is
 *  run, in time order. Called with no lock held. */
void advanceTo(int64_t t);

/** The ether's welcome: the run's mode, T at join, the epoch. */
void welcome(bool isVirtualRun, int64_t t, int64_t epochUs, uint64_t seq);

/** A timed message has been applied: this station owes the ether an idle for
 *  `seq`. */
void granted(uint64_t seq);

/** This station sent the ether something other than an idle, which the ether
 *  takes as no longer idle. */
void spoke();

/* ---- The host's side ---- */

int  wakeCreate(void (*due)(void*), void* arg);
void wakeAt(int wake, int64_t nodeUs);
void idle();

/** `moved` runs each time a grant moves T, on the thread that applied it,
 *  with no lock held, before anything due at the new T runs. */
void onAdvance(void (*moved)(void));

/** The idle message itself, the link's to send. */
void setIdleSender(void (*send)(uint64_t seq, int64_t until));

/** The station has opened its link: in a virtual run, the busy watchdog and
 *  the time shim come up. */
void start();

}  // namespace conductor
