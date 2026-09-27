"""mixed: the Reticulous stations get an admin login and an LXMF identity."""


async def setup(node):
    if node.kind == "reticulous":
        await node.run("auth passwd admin admin")
        await node.run("lxmf create {name}")
