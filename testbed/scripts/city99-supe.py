"""city99 with SUPE on channel plan 1: city99-lora's setup, then SUPE on."""


async def setup(node):
    await node.run("auth passwd admin admin")
    await node.run("lxmf create {name}")
    await node.run("set s.rnsd.dir.persist_routes 1")
    await node.run("set s.rnsd.dir.persist_s 60")
    await node.run("set s.lora.0.SUPE.afa 1")
    await node.run("lora 0 supe enable")
