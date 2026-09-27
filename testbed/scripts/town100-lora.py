"""town100, plain LoRa; written by gen_town100.py."""

LINES = [
    "auth passwd admin admin",
    "lxmf create {name}",
    "set s.lora.0.community_radius 25",
    "set s.rnsd.dir.persist_routes 1",
    "set s.rnsd.dir.persist_s 60",
]


async def setup(node):
    for line in LINES:
        await node.run(line)
