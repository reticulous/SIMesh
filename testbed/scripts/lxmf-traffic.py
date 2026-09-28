"""LXMF traffic: warm-up announces, an hour of messages, a drain; the record in the run.

Every station runs Reticulous's dev build, is set up by the startup script
(its role, its radio, its nodeset's own setup), and gets an LXMF identity at
its first boot; its messages go out as the `send_msg`
meta command (simesh.traffic), so the driver runs on any firmware that has
one. `OPTIONS` below are the phases' settings, over simesh.traffic's
defaults; the result, every send with its route and reply, lands in the run
directory as `traffic.json`, the simulation is paused, and the report is the
delivery `delivery.py traffic.json <run>` counts.
"""
from simesh import *
from simesh import traffic

OPTIONS = {
    "warm_rounds": 3,
    "traffic": 3600.0,
    "every": 5.0,
    "seed": 17,
    "drain": 600.0,
}

time("max")
firmware("all", "reticulous_dev_latest")
include("scripts/startup.py")
# What a station needs to take part: a sender and a recipient each need an
# identity.
on_first_boot("all", """
    lxmf create {name}
""")

traffic.run(OPTIONS)
pause()


def report(run_dir):
    return traffic.report(run_dir)
