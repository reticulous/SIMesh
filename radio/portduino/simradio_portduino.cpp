/**
 * simradio for a Portduino firmware: the SX1262 on SPI and its four lines,
 * and the firmware's idle wait, on the simulated chip.
 *
 * A firmware built on Portduino (Meshtastic's Arduino API for Linux) links
 * this library and calls two hooks, each a weak function of its own whose
 * strong definition is here:
 *
 *   native_radio_backend_init()   once, before its SPI.begin() and before it
 *                                 touches the radio's pins
 *   rnode_idle(max_ms)            where it has nothing to do for up to max_ms
 *
 * The init opens the station's link to the ether and chip slot 0, installs an
 * SPIChip into the global SPI that hands each transfer to the chip whole, and
 * binds the chip's pins with gpioBind: NSS (nothing to do, every transfer is
 * already one frame), RESET (its rising edge resets the chip), BUSY and DIO1
 * (the levels the chip drives). Binding a pin also turns off Portduino's
 * 100 ms sleep between loop() calls, so the firmware's own idle is the only
 * wait in its loop.
 *
 * The idle waits on a condition variable until max_ms has passed or DIO1 has
 * risen; the time shim answers the wait in node time, and Portduino's next
 * gpioIdle() runs the DIO1 interrupt handler.
 *
 * Environment: SIMESH_NODE_ID, SIMESH_BIND_ADDR and SIMESH_ETHER (the station
 * contract), and the pin numbers the firmware drives the chip on:
 * SIMRADIO_PIN_NSS, SIMRADIO_PIN_RESET, SIMRADIO_PIN_BUSY, SIMRADIO_PIN_DIO1
 * (1, 2, 3 and 4 when unset).
 */
#include <Arduino.h>
#include <PortduinoGPIO.h>
#include <SPIChip.h>

#include "simradio.h"

#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <pthread.h>
#include <sys/time.h>
#include <vector>

namespace {

simradio_t* s_chip;

pthread_mutex_t s_mu = PTHREAD_MUTEX_INITIALIZER;
pthread_cond_t  s_cv = PTHREAD_COND_INITIALIZER;   /* on the wall clock, as the shim reads it */
bool s_rose;                    /* DIO1 has risen since the last idle returned */
volatile int s_dio1;
volatile int s_busy;

void onPin(void*, int pin, int level)
{
    if (pin == SIMRADIO_PIN_BUSY) {
        s_busy = level;
        return;
    }
    if (pin != SIMRADIO_PIN_DIO1)
        return;
    pthread_mutex_lock(&s_mu);
    if (level && !s_dio1) {
        s_rose = true;
        pthread_cond_signal(&s_cv);
    }
    s_dio1 = level;
    pthread_mutex_unlock(&s_mu);
}

int envInt(const char* name, int fallback)
{
    const char* v = getenv(name);
    return v && *v ? atoi(v) : fallback;
}

/* Each transfer is one frame, NSS low to NSS high. The driver transfers in
 * place, so the frame going out is copied before the reply overwrites it. */
class SimChip : public SPIChip {
public:
    int transfer(const uint8_t* out, uint8_t* in, size_t len, bool) override
    {
        out_.assign(len, 0);
        if (out)
            memcpy(out_.data(), out, len);
        in_.assign(len, 0);
        simradio_transfer(s_chip, out_.data(), len, in_.data());
        if (in)
            memcpy(in, in_.data(), len);
        return 0;
    }

private:
    std::vector<uint8_t> out_, in_;
};

/* HardwareSPI keeps its chip protected; a pointer to the member, taken
 * through a derived class, reaches it on the global SPI. */
struct SpiAccess : HardwareSPI {
    static void install(HardwareSPI& spi, std::shared_ptr<SPIChip> chip)
    {
        spi.*(&SpiAccess::spiChip) = std::move(chip);
    }
};

class NssPin : public GPIOPin {
public:
    NssPin(pin_size_t n) : GPIOPin(n, "NSS") { setSilent(); }
};

class ResetPin : public GPIOPin {
public:
    ResetPin(pin_size_t n) : GPIOPin(n, "RESET") { setSilent(); }
    void writePin(PinStatus s) override
    {
        bool rising = s == HIGH && level_ == LOW;
        level_ = s;
        GPIOPin::writePin(s);
        if (rising)
            simradio_reset(s_chip);
    }

private:
    PinStatus level_ = HIGH;
};

class LevelPin : public GPIOPin {
public:
    LevelPin(pin_size_t n, const char* name, volatile int* level)
        : GPIOPin(n, name), level_(level) { setSilent(); }

protected:
    PinStatus readPinHardware() override { return *level_ ? HIGH : LOW; }

private:
    volatile int* level_;
};

}  // namespace

void native_radio_backend_init()
{
    int sid = envInt("SIMESH_NODE_ID", 0);
    const char* bind = getenv("SIMESH_BIND_ADDR");
    const char* ether = getenv("SIMESH_ETHER");
    if (simradio_station_open(sid, bind ? bind : "127.0.0.1", ether ? ether : "") != 0) {
        fprintf(stderr, "[simradio] could not open the link to the ether at %s\n",
                ether ? ether : "(none)");
        exit(1);
    }
    s_chip = simradio_open(0, onPin, nullptr);
    if (!s_chip) {
        fprintf(stderr, "[simradio] could not open chip slot 0\n");
        exit(1);
    }
    s_busy = simradio_pin(s_chip, SIMRADIO_PIN_BUSY);
    s_dio1 = simradio_pin(s_chip, SIMRADIO_PIN_DIO1);

    SpiAccess::install(SPI, std::make_shared<SimChip>());
    gpioBind(new NssPin(envInt("SIMRADIO_PIN_NSS", 1)));
    gpioBind(new ResetPin(envInt("SIMRADIO_PIN_RESET", 2)));
    gpioBind(new LevelPin(envInt("SIMRADIO_PIN_BUSY", 3), "BUSY", &s_busy));
    gpioBind(new LevelPin(envInt("SIMRADIO_PIN_DIO1", 4), "DIO1", &s_dio1));
}

void rnode_idle(uint32_t max_ms)
{
    if (max_ms == 0)
        return;
    struct timeval now;
    gettimeofday(&now, nullptr);
    int64_t end_us = (int64_t)now.tv_sec * 1000000 + now.tv_usec + (int64_t)max_ms * 1000;
    struct timespec end = {(time_t)(end_us / 1000000), (long)(end_us % 1000000) * 1000};

    pthread_mutex_lock(&s_mu);
    while (!s_rose) {
        if (pthread_cond_timedwait(&s_cv, &s_mu, &end) == ETIMEDOUT)
            break;
    }
    s_rose = false;
    pthread_mutex_unlock(&s_mu);
}
