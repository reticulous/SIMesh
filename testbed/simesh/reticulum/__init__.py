"""Reticulum's parts of the library:

    simesh.reticulum.frames     what a frame on the air is: the RNode header,
                                the Reticulum packet, SUPE's frames, the power
                                request
    simesh.reticulum.traffic    the LXMF traffic driver
    simesh.reticulum.delivery   delivery of a driven run, from the senders' logs

A Reticulous station is one whose device is of kind `reticulous`. Reticulum
has no routers or repeaters of its own; a transport is what forwards.
"""

KIND_TYPES = ("reticulous",)
