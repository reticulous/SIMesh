# simradio for a Portduino firmware

A PlatformIO library that puts SIMesh's simulated SX1262 (`radio/`) under a
firmware built on Portduino, Meshtastic's Arduino API for Linux, in place of
`/dev/spidev` and libgpiod. The firmware's radio driver is unchanged: it
talks SPI and pins as on a board, and each transfer reaches the chip model
whole.

```
firmware ── SPI.transfer(frame) ─────► SimChip ── simradio_transfer ──► chip model
firmware ── digitalWrite(RESET) ─────► ResetPin: rising edge ── simradio_reset
firmware ◄── digitalRead(BUSY/DIO1) ── LevelPin ◄── on_pin ── chip model
firmware ── rnode_idle(max_ms) ──────► wait until max_ms or DIO1 rises (node time)
```

## What a firmware calls

Two functions, each a weak function of the firmware's own whose strong
definition is here:

- `void native_radio_backend_init()`, once, after the firmware knows its
  pins and before its `SPI.begin()` and its first touch of the radio. It
  opens the station's link to the ether (`SIMESH_NODE_ID`,
  `SIMESH_BIND_ADDR`, `SIMESH_ETHER`) and chip slot 0, installs its `SPIChip`
  into the global `SPI`, and binds the chip's lines with `gpioBind`.
- `void rnode_idle(uint32_t max_ms)`, where the firmware has nothing to do
  for up to `max_ms`. It waits on a condition variable until then or until
  DIO1 rises; in a virtual-time run the time shim answers the wait in node
  time, and Portduino's next `gpioIdle()` runs the DIO1 interrupt handler.
  `max_ms == 0` returns at once.

The firmware must hand every SPI transaction to `SPI.transfer` as one frame,
NSS low to NSS high, which is what a driver written for spidev already does.
Binding a pin also turns off Portduino's 100 ms sleep between `loop()` calls,
so the firmware's own idle is the only wait in its loop.

## Its pins

The chip's four lines are bound at `SIMRADIO_PIN_NSS`, `SIMRADIO_PIN_RESET`,
`SIMRADIO_PIN_BUSY` and `SIMRADIO_PIN_DIO1` (1, 2, 3 and 4 when unset), which
must be the pins the firmware drives the chip on. NSS does nothing, since
each frame is already one transfer.

## Building with it

`simesh build radio` builds `radio/build/libsimradio.a`, which `link.py`
links into the program by path. A firmware's environment lists the library
with a `symlink://` entry in `lib_deps`. `casefold.py`, as a `pre:` extra
script, lets Portduino build on a case-insensitive filesystem, where its
`String.h` would otherwise answer `#include <string.h>`. The library is
linked as objects, not as an archive, so its strong definitions win over the
firmware's weak ones.

attermann's microReticulum_Firmware is built with it as `[env:simesh]`
(the `microreticulum` kind).
