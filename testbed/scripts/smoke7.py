"""smoke7 and mitte7: an internet gateway, LoRa stations in the netgraph
community, and TCP peers of the gateway, by tag; rnsd logging at debug."""

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
    await node.run("auth passwd admin admin")
    await node.run("log rnsd debug")
    if "lora" in node.tags:
        await node.run("set s.netgraph.community city")
        await node.run("set s.netgraph.passphrase citymesh-passphrase")
    if "tcp-peer" in node.tags:
        await node.peer_tcp("internet", 4965)
    if node.name == "internet":
        for line in GATEWAY:
            await node.run(line)
    if "lxmf" in node.tags or node.name == "internet":
        await node.run("lxmf create {name}")
