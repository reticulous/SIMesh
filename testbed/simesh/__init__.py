"""The library scripts and the analysis tools share.

    simesh.sim          a driver's hold on a running simulation: its stations,
                        its clock, selections of stations and what to do with
                        them (lines, intents, resets), snapshots, moves
    simesh.setup        one station as a script's `setup(node)` sees it
    simesh.runner       a script's `main` run on a simulation
    simesh.view         a run directory as the analysis tools see it: nodes by id
                        and name, positions in the geodata's metres, each node's
                        declared radio and role, and the medium's levels from the
                        run's own loss tables
    simesh.record       the ether's record, line by line
    simesh.reticulum    Reticulum's parts: what a frame on the air is (the
                        Reticulum packet, SUPE's frames), the LXMF traffic
                        driver and its delivery analysis

`start`, `attach`, `Sim` and `Selection` are here at the top, so a script
needs only `import simesh`.

What is generic stays out of a protocol: the record, per-carrier airtime,
link geometry, the loss table and the levels it gives. What a frame means is
a protocol's, found by the station's kind (its device's `kind`) through
`protocol_for`.

A **role** is what a station does for the others, one of `ROLES`: a
`transport`, `router` or `repeater` forwards (`FORWARDING`), a `client`
only speaks for itself.
"""

from simesh import reticulum
from simesh.sim import Selection, Sim, SimError, attach, start  # noqa: F401 - the library's face

ROLES = ("transport", "router", "repeater", "client")
FORWARDING = ("transport", "router", "repeater")

# The radio a station is taken to have when its node declares none: the
# calling channel of the EU 868 plan at SF8, 125 kHz, 14 dBm.
DEFAULT_FREQ_HZ = 869_525_000
DEFAULT_SF = 8
DEFAULT_BW_HZ = 125_000
DEFAULT_POWER_DBM = 14.0

PROTOCOLS = (reticulum,)


def protocol_for(kind_type):
    """The protocol module that reads stations of this kind type, or None."""
    for module in PROTOCOLS:
        if kind_type in module.KIND_TYPES:
            return module
    return None
