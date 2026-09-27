"""smoke4: an admin login everywhere; SUPE, LXMF and RNode-over-TCP by tag."""


async def setup(node):
    await node.run("auth passwd admin admin")
    if "supe" in node.tags:
        await node.run("set s.lora.0.SUPE.afa 1")
        await node.run("set s.lora.0.SUPE.enable 1")
    if "lxmf" in node.tags:
        await node.run("lxmf create {name}")
    if "rnode-tcp" in node.tags:
        await node.run("set s.lora.rnode.tcp 1")
