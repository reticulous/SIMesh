"""Real time: every node on the dev build, set up by the startup script, and left running.

The simulation runs on the wall clock and keeps running when this script
ends, for the page, the consoles and the stations' web UIs.
"""
from simesh import *

time("real")
firmware("all", "reticulous_dev_latest")
include("scripts/startup.py")

up("all")
