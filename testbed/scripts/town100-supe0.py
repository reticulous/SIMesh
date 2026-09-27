"""town100, SUPE, channel plan 0; written by gen_town100.py."""

LINES = [
    "auth passwd admin admin",
    "lxmf create {name}",
    "set s.lora.0.community_radius 25",
    "set s.rnsd.dir.persist_routes 1",
    "set s.rnsd.dir.persist_s 60",
    "set s.lora.0.SUPE.afa 0",
    "lora 0 supe enable",
]


async def setup(node):
    for line in LINES:
        await node.run(line)
