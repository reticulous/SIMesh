"""city99 on plain LoRa: an admin login, an LXMF identity, routes kept across resets."""


async def setup(node):
    await node.run("auth passwd admin admin")
    await node.run("lxmf create {name}")
    await node.run("set s.rnsd.dir.persist_routes 1")
    await node.run("set s.rnsd.dir.persist_s 60")
