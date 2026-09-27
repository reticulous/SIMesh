"""soak30: the Reticulous stations get an admin login and an LXMF identity,
and SUPE where tagged."""


async def setup(node):
    if node.kind != "reticulous":
        return
    await node.run("auth passwd admin admin")
    await node.run("lxmf create {name}")
    if "supe" in node.tags:
        await node.run("lora 0 supe enable")
