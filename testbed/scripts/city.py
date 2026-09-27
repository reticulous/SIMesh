"""citye and cityf: a city mesh with an internet gateway, by tag.

`lora` nodes join the netgraph community, `supe` ones run SUPE with lora
debug logging, `tcp-peer` ones peer with `internet` over TCP, `rnode-tcp`
ones serve their radio as an RNode over TCP, and `internet` is the TCP
access point. Sergeyculum stations are given the host's time.
"""

GATEWAY = [
    "set s.tcp.servers.0.id 1",
    "set s.tcp.servers.0.enable 1",
    "set s.tcp.servers.0.port 4965",
    "set s.tcp.servers.0.mode access_point",
    "set s.tcp.servers.0.max_conns 16",
    "set s.tcp.servers.0.upnp 0",
    "set s.tcp.servers.0.community_radius 64",
]


async def setup(node):
    if node.kind == "berlinmesh":
        await node.run("time set now")
        return
    await node.run("auth passwd admin admin")
    await node.run("lxmf create {name}")
    if "lora" in node.tags:
        await node.run("set s.netgraph.community city")
        await node.run("set s.netgraph.passphrase citymesh-passphrase")
    if "supe" in node.tags:
        await node.run("set s.lora.0.SUPE.afa 1")
        await node.run("lora 0 supe enable")
        await node.run("log lora debug")
    if "tcp-peer" in node.tags:
        await node.peer_tcp("internet", 4965)
    if "rnode-tcp" in node.tags:
        await node.run("set s.lora.rnode.tcp 1")
    if node.name == "internet":
        for line in GATEWAY:
            await node.run(line)
