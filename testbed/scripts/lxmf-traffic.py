"""LXMF traffic: warm-up announces, an hour of messages, a drain; the record in the run.

Run on a new simulation, its own setup gives every station a device password
and an LXMF identity; on a running one, or with another setup script
(town100 with town100-lora, say), that one must. `OPTIONS` below are the phases' settings,
over simesh.reticulum.traffic's defaults; the result, every send with its
route and reply, lands in the run directory as `traffic.json`, and its
report is the delivery `delivery.py traffic.json <run>` counts.
"""
import os

from simesh.reticulum import traffic

OPTIONS = {
    "warm_rounds": 3,
    "traffic": 3600.0,
    "every": 5.0,
    "seed": 17,
    "drain": 600.0,
}


async def setup(node):
    # What a station needs to take part: Reticulum runs only once a device
    # password is set, and a sender and a recipient each need an identity.
    # Run with another setup script, that one's setup is used instead.
    await node.run("auth passwd admin admin")
    await node.run("lxmf create {name}")


async def main(sim):
    await traffic.run_on(sim, OPTIONS, os.path.join(sim.run_dir, "traffic.json"))


def report(run_dir):
    return traffic.report(run_dir, os.path.join(run_dir, "traffic.json"))
