"""Every world's start: roles, radios, each nodeset's own setup, then the radios up.

Included by a script (`include("scripts/startup.py")`) among its
declarations: all of this is said to a station at its first boot, in the
order written, in the station's own kind's lines. A script's own first-boot
lines after the include come after the radio is up.
"""
from simesh import *
from globals import FREQ_MHZ, SF, BW_KHZ, CR, SYNC

RADIOS = ~nodes(tag="no-radio")

# A node tagged with a role is told it.
on_first_boot(nodes(tag="transport"), role("transport"))

# Every node's radio, bar those tagged no-radio, at the node's own maximum power.
on_first_boot(RADIOS, radio(freq_mhz=FREQ_MHZ, sf=SF, bw_khz=BW_KHZ, cr=CR, sync=SYNC,
                            tx_dbm="max"))

# What only one nodeset's nodes need, from its own nodesets/<name>.py.
for nodeset in nodesets():
    include("nodesets/%s.py" % nodeset, missing_ok=True)

# Last, because a radio reads its settings, SUPE and the rest when it starts.
on_first_boot(RADIOS, radio_up())
