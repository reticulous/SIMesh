"""Devices: listings, expansion, refresh from a directory and over HTTP,
imports, local devices, and what a node's `device:` resolves to."""

import asyncio
import os
import sys
import zipfile

import pytest
import yaml

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import devices  # noqa: E402

ARCH = "aarch64"
OTHER = "x86_64"


def package_zip(path, arch=ARCH, stamp="20260925035045", kind="reticulous",
                extra=None, node=None):
    doc = node if node is not None else {
        "kind": kind, "arch": arch, "stamp": stamp, "elf": "reticulous.elf",
        "fixed": "fixed", "project": "Reticulous", "catalogue": "dev",
        "entry": "hw-simesh-%s" % arch}
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("node.yaml", yaml.safe_dump(doc))
        info = zipfile.ZipInfo("reticulous.elf")
        info.external_attr = 0o644 << 16
        zf.writestr(info, "#!/bin/sh\necho station\n")
        zf.writestr("fixed/webroot/index.html", "<p>hi</p>")
        for name, body in (extra or {}).items():
            zf.writestr(name, body)
    return path


def catalogue(directory, images, attrs=""):
    """A catalogue directory: the given zips and an index.html listing them,
    plus a board image that is not a device file."""
    os.makedirs(directory, exist_ok=True)
    rows = []
    for name, arch, stamp in images:
        package_zip(os.path.join(directory, name), arch=arch, stamp=stamp)
        rows.append('<li><a href="%s"%s>%s</a> <small>(1 KB)</small></li>'
                    % (name, attrs, name))
    board = "reticulous_hw-heltecv4_20260925035045.zip"
    with open(os.path.join(directory, board), "wb") as f:
        f.write(b"not a device")
    rows.append('<li><a href="%s" data-onboarding="device">%s</a></li>' % (board, board))
    with open(os.path.join(directory, "index.html"), "w") as f:
        f.write("<!doctype html>\n<title>x</title>\n<ul>\n%s\n</ul>\n" % "\n".join(rows))
    return directory


def quiet(lines):
    return lines.append


# ---- names and listings ------------------------------------------------------

def test_an_image_name_splits_from_the_right():
    assert devices.split_image_name("reticulous_hw-simesh-aarch64_20260925035045.zip") == \
        ("reticulous", "hw-simesh-aarch64", "20260925035045")
    assert devices.split_image_name("reticulous_odd_entry_name_1.zip") == \
        ("reticulous", "odd_entry_name", "1")
    assert devices.split_image_name("reticulous_generic_abc.zip") is None
    assert devices.split_image_name("index.html") is None
    assert devices.entry_arch("hw-simesh-x86_64") == "x86_64"
    assert devices.entry_arch("hw-heltecv4") is None


def test_the_newest_packages_per_entry_are_picked_and_boards_are_left_out():
    links = devices.parse_listing(
        '<a href="r_hw-simesh-aarch64_2.zip" data-target="linux">a</a>'
        '<a href="r_hw-simesh-aarch64_3.zip">b</a>'
        '<a href="r_hw-simesh-aarch64_1.zip">b</a>'
        '<a href="r_hw-simesh-aarch64_4.zip">b</a>'
        '<a href="r_hw-simesh-x86_64_1.zip">c</a>'
        '<a href="r_hw-heltecv4_9.zip">d</a><a name="x">no href</a>')
    newest = devices.newest_packages(links)
    assert sorted(newest) == ["hw-simesh-aarch64", "hw-simesh-x86_64"]
    assert [p["stamp"] for p in newest["hw-simesh-aarch64"]] == ["4", "3", "2"]
    assert devices.newest_packages(links[:1])["hw-simesh-aarch64"][0]["attrs"]["data-target"] == "linux"


def test_a_source_is_a_name_a_url_or_a_directory(tmp_path):
    s = devices.Source("stable", "https://example.net/builds/")
    assert (s.name, s.location, s.local) == ("stable", "https://example.net/builds/stable/", False)
    s = devices.Source("https://github.com/o/r/releases/download/catalogue-dev")
    assert s.name == "dev" and s.location.endswith("catalogue-dev/")
    d = catalogue(str(tmp_path / "rop"), [])
    s = devices.Source(d)
    assert (s.name, s.local) == ("rop", True)
    with pytest.raises(devices.DeviceError):
        devices.Source(str(tmp_path / "nowhere" / "x"))
    with pytest.raises(devices.DeviceError, match="cannot be called"):
        devices.Source("imported")
    assert devices.Source(catalogue(str(tmp_path / "local"), [])).name == "builds-local"


def test_the_builds_beside_simesh_are_sources(tmp_path):
    catalogue(str(tmp_path / "builds" / "dev"), [])
    os.makedirs(str(tmp_path / "builds" / "elf"))
    assert devices.builds_sources(str(tmp_path / "builds")) == [str(tmp_path / "builds" / "dev")]
    assert devices.builds_sources(str(tmp_path / "nowhere")) == []


def test_a_device_is_called_by_its_name_or_its_project_catalogue_and_time():
    assert devices.display_name({"name": "Mine", "stamp": "1"}) == "Mine"
    assert devices.display_name({"project": "Reticulous", "stamp": "20260925035045"}, "dev") == \
        "Reticulous dev 2026-09-25 03:50"


# ---- expanding ---------------------------------------------------------------

def test_expanding_checks_node_yaml_and_makes_the_elf_and_tools_executable(tmp_path):
    node = {"kind": "berlinmesh", "arch": ARCH, "stamp": "20260925035045",
            "elf": "reticulous.elf", "tools": {"rncfg": "bin/rncfg"}, "env": {"A": "./x", "B": "y"},
            "stands_for": "nRF52840"}
    z = package_zip(str(tmp_path / "p.zip"), node=node, extra={"bin/rncfg": "tool"})
    dest = str(tmp_path / "devices" / "dev" / "sergey_hw-simesh-aarch64_20260925035045")
    devices.expand(z, dest, {"catalogue": "dev", "url": z}, ARCH)
    assert os.access(os.path.join(dest, "reticulous.elf"), os.X_OK)
    assert os.access(os.path.join(dest, "bin", "rncfg"), os.X_OK)
    assert devices.read_origin(dest)["catalogue"] == "dev"
    got = devices.package_result(dest)
    assert got["tools"] == {"rncfg": os.path.join(dest, "bin", "rncfg")}
    assert got["env"] == {"A": os.path.join(dest, "x"), "B": "y"}
    assert got["stands_for"] == "nRF52840" and got["kind_type"] == "berlinmesh"
    assert not [n for n in os.listdir(os.path.dirname(dest)) if n.startswith(".part-")]


@pytest.mark.parametrize("zip_kwargs, why", [
    ({"arch": OTHER}, "this machine is"),
    ({"stamp": "20260101000000"}, "the filename"),
    ({"extra": {"../escape": "x"}}, "leaves the package"),
    ({"node": {"kind": "reticulous", "arch": ARCH, "stamp": "20260925035045"}}, "no `elf`"),
    ({"node": {"kind": "reticulous", "arch": ARCH, "stamp": "20260925035045",
               "elf": "missing.elf"}}, "not in the package"),
    ({"node": {"kind": "reticulous", "arch": ARCH, "stamp": "20260925035045",
               "elf": "reticulous.elf", "tools": {"t": "gone"}}}, "tool gone"),
])
def test_a_bad_package_leaves_nothing_behind(tmp_path, zip_kwargs, why):
    z = package_zip(str(tmp_path / "p.zip"), **zip_kwargs)
    dest = str(tmp_path / "devices" / "dev" / "reticulous_hw-simesh-aarch64_20260925035045")
    with pytest.raises(devices.DeviceError, match=why):
        devices.expand(z, dest, {}, ARCH)
    assert os.listdir(os.path.dirname(dest)) == []


def test_an_imported_zip_of_any_name_is_named_from_its_node_yaml(tmp_path):
    dd = str(tmp_path / "devices")
    z = package_zip(str(tmp_path / "upload.zip"))
    got = devices.import_zip(z, "whatever.zip", dd, ARCH)
    assert got["ref"] == "imported/reticulous_hw-simesh-aarch64_20260925035045"
    assert got["catalogue"] == "imported"
    assert devices.resolve("imported", devices_dir=dd, arch=ARCH)["ref"] == got["ref"]
    with pytest.raises(devices.DeviceError, match="already"):
        devices.import_zip(z, "again.zip", dd, ARCH)
    other = package_zip(str(tmp_path / "other.zip"), arch=OTHER)
    with pytest.raises(devices.DeviceError, match="built for x86_64"):
        devices.import_zip(other, "other.zip", dd, ARCH)
    mesh = package_zip(str(tmp_path / "m.zip"), node={
        "kind": "berlinmesh", "arch": ARCH, "stamp": "7", "elf": "reticulous.elf"})
    assert devices.import_zip(mesh, "m.zip", dd, ARCH)["ref"] == \
        "imported/berlinmesh_hw-simesh-aarch64_7"
    bare = str(tmp_path / "bare.zip")
    with zipfile.ZipFile(bare, "w") as zf:
        zf.writestr("x", "y")
    with pytest.raises(devices.DeviceError, match="no node.yaml"):
        devices.import_zip(bare, "bare.zip", dd, ARCH)


# ---- refresh -----------------------------------------------------------------

def test_refresh_from_a_directory_fetches_this_arch_and_skips_the_other(tmp_path):
    src = catalogue(str(tmp_path / "builds" / "dev"), [
        ("reticulous_hw-simesh-aarch64_20260925035045.zip", ARCH, "20260925035045"),
        ("reticulous_hw-simesh-x86_64_20260925035045.zip", OTHER, "20260925035045"),
    ], attrs=' data-target="linux"')
    dd = str(tmp_path / "devices")
    said = []
    results = asyncio.run(devices.refresh([src], dd, ARCH, said.append))
    states = {r["entry"]: r["state"] for r in results}
    assert states == {"hw-simesh-aarch64": "fetched", "hw-simesh-x86_64": "skipped"}
    assert any("skipping hw-simesh-x86_64" in line for line in said)
    assert os.listdir(os.path.join(dd, "dev")) == ["reticulous_hw-simesh-aarch64_20260925035045"]

    again = asyncio.run(devices.refresh([src], dd, ARCH, said.append))
    assert {r["entry"]: r["state"] for r in again}["hw-simesh-aarch64"] == "current"


def test_refresh_keeps_the_newest_three_per_entry(tmp_path):
    dd = str(tmp_path / "devices")
    src = str(tmp_path / "builds" / "stable")
    for stamp in ("20260101000000", "20260201000000", "20260301000000", "20260401000000"):
        # make-builds leaves one image per entry, so each round replaces it.
        catalogue(src, [("reticulous_hw-simesh-aarch64_%s.zip" % stamp, ARCH, stamp)])
        asyncio.run(devices.refresh([src], dd, ARCH, quiet([])))
    assert sorted(os.listdir(os.path.join(dd, "stable"))) == [
        "reticulous_hw-simesh-aarch64_20260201000000",
        "reticulous_hw-simesh-aarch64_20260301000000",
        "reticulous_hw-simesh-aarch64_20260401000000"]


def test_refresh_fetches_the_newest_three_and_not_what_would_be_pruned(tmp_path):
    dd = str(tmp_path / "devices")
    stamps = ("20260101000000", "20260201000000", "20260301000000", "20260401000000")
    web = catalogue(str(tmp_path / "web" / "dev"),
                    [("reticulous_hw-simesh-aarch64_%s.zip" % s, ARCH, s) for s in stamps[:3]])
    asyncio.run(devices.refresh([web], dd, ARCH, quiet([])))
    assert len(os.listdir(os.path.join(dd, "dev"))) == 3
    mine = catalogue(str(tmp_path / "builds" / "dev"),
                     [("reticulous_hw-simesh-aarch64_%s.zip" % stamps[3], ARCH, stamps[3])])
    asyncio.run(devices.refresh([mine], dd, ARCH, quiet([])))
    again = asyncio.run(devices.refresh([web], dd, ARCH, quiet([])))
    assert [r["state"] for r in again] == ["current", "current", "skipped"]
    assert sorted(os.listdir(os.path.join(dd, "dev")))[0].endswith(stamps[1])


def test_one_unreadable_source_does_not_stop_the_others(tmp_path):
    src = catalogue(str(tmp_path / "dev"), [
        ("reticulous_hw-simesh-aarch64_5.zip", ARCH, "5")])
    results = asyncio.run(devices.refresh(["./not/a/catalogue", src],
                                          str(tmp_path / "devices"), ARCH, quiet([])))
    assert [r["state"] for r in results] == ["failed", "fetched"]


def test_refresh_over_http(tmp_path):
    from aiohttp import web

    served = catalogue(str(tmp_path / "site" / "dev"), [
        ("reticulous_hw-simesh-aarch64_7.zip", ARCH, "7")])
    dd = str(tmp_path / "devices")

    async def go():
        app = web.Application()
        app.router.add_static("/builds/", str(tmp_path / "site"))
        runner = web.AppRunner(app)
        await runner.setup()
        site = web.TCPSite(runner, "127.0.0.1", 0)
        await site.start()
        port = site._server.sockets[0].getsockname()[1]
        try:
            return await devices.refresh(["dev"], dd, ARCH, quiet([]),
                                         base="http://127.0.0.1:%d/builds/" % port)
        finally:
            await runner.cleanup()

    results = asyncio.run(go())
    assert [r["state"] for r in results] == ["fetched"]
    got = devices.resolve("dev", devices_dir=dd, arch=ARCH)
    assert got["source"].endswith("/builds/dev/reticulous_hw-simesh-aarch64_7.zip")
    assert os.path.isdir(served)


# ---- resolve -----------------------------------------------------------------

def installed_devices(tmp_path):
    dd = str(tmp_path / "devices")
    for cat, stamp, arch in (("stable", "20260901000000", ARCH),
                             ("dev", "20260901000000", ARCH),
                             ("dev", "20260920000000", ARCH),
                             ("dev", "20260930000000", OTHER)):
        name = "reticulous_hw-simesh-%s_%s" % (arch, stamp)
        z = package_zip(str(tmp_path / (name + ".zip")), arch=arch, stamp=stamp)
        devices.expand(z, os.path.join(dd, cat, name), {"catalogue": cat}, arch)
    return dd


def test_a_catalogue_name_resolves_to_its_newest_package_for_this_arch(tmp_path):
    dd = installed_devices(tmp_path)
    got = devices.resolve("dev", devices_dir=dd, arch=ARCH)
    assert got["stamp"] == "20260920000000" and got["catalogue"] == "dev"
    assert got["kind_type"] == "reticulous" and got["arch"] == ARCH
    assert got["name"] == "Reticulous dev 2026-09-20 00:00"
    assert got["elf"].endswith("reticulous.elf") and os.path.isdir(got["fixed"])
    assert devices.resolve("stable", devices_dir=dd, arch=ARCH)["stamp"] == "20260901000000"
    with pytest.raises(devices.DeviceError, match="simesh devices refresh rop"):
        devices.resolve("rop", devices_dir=dd, arch=ARCH)


def test_a_package_or_a_stamp_resolves(tmp_path):
    dd = installed_devices(tmp_path)
    name = "reticulous_hw-simesh-aarch64_20260901000000"
    for cat in ("stable", "dev"):
        got = devices.resolve("%s/%s" % (cat, name), devices_dir=dd, arch=ARCH)
        assert got["ref"] == "%s/%s" % (cat, name) and got["catalogue"] == cat
    with pytest.raises(devices.DeviceError, match="several catalogues"):
        devices.resolve(name, devices_dir=dd, arch=ARCH)
    only = "reticulous_hw-simesh-aarch64_20260920000000"
    assert devices.resolve(only, devices_dir=dd, arch=ARCH)["ref"] == "dev/" + only
    with pytest.raises(devices.DeviceError, match="built for x86_64"):
        devices.resolve("reticulous_hw-simesh-x86_64_20260930000000", devices_dir=dd, arch=ARCH)
    assert devices.resolve("20260920000000", devices_dir=dd, arch=ARCH)["catalogue"] == "dev"
    assert devices.resolve(20260920000000, devices_dir=dd, arch=ARCH)["catalogue"] == "dev"
    with pytest.raises(devices.DeviceError, match="several catalogues"):
        devices.resolve("20260901000000", devices_dir=dd, arch=ARCH)
    with pytest.raises(devices.DeviceError, match="no aarch64 package"):
        devices.resolve("20260930000000", devices_dir=dd, arch=ARCH)


def test_the_listing_says_which_package_each_catalogue_resolves_to(tmp_path):
    dd = installed_devices(tmp_path)
    rows = {r["ref"]: r for r in devices.listing(dd, ARCH)}
    assert rows["dev/reticulous_hw-simesh-aarch64_20260920000000"]["newest_of"] == "dev"
    assert rows["dev/reticulous_hw-simesh-aarch64_20260901000000"]["newest_of"] is None
    assert rows["stable/reticulous_hw-simesh-aarch64_20260901000000"]["newest_of"] == "stable"
    assert "dev/reticulous_hw-simesh-x86_64_20260930000000" not in rows


def test_the_listing_shows_the_newest_three_of_a_catalogue(tmp_path):
    dd = str(tmp_path / "devices")
    for stamp in ("20260101000000", "20260201000000", "20260301000000", "20260401000000"):
        z = package_zip(str(tmp_path / ("%s.zip" % stamp)), stamp=stamp)
        devices.expand(z, os.path.join(dd, "dev", "reticulous_hw-simesh-aarch64_%s" % stamp),
                       {"catalogue": "dev"}, ARCH)
    shown = [r["stamp"] for r in devices.listing(dd, ARCH)]
    assert shown == ["20260401000000", "20260301000000", "20260201000000"]


def test_a_local_device_names_a_build_anywhere(tmp_path):
    dd = tmp_path / "devices"
    build = tmp_path / "ws" / "fw" / "target"
    build.mkdir(parents=True)
    (build / "simesh").write_text("elf")
    (build / "rncfg").write_text("tool")
    (dd / "local").mkdir(parents=True)
    (dd / "local" / "sergey.yaml").write_text(
        "kind: berlinmesh\nname: Sergey's\nstands_for: nRF52840\n"
        "elf: ../../ws/fw/target/simesh\ntools: { rncfg: ../../ws/fw/target/rncfg }\n")
    (dd / "local" / "broken.yaml").write_text("kind: berlinmesh\nelf: ../../nowhere\n")
    got = devices.resolve("sergey", devices_dir=str(dd), arch=ARCH)
    assert got["elf"] == str(build / "simesh")
    assert got["tools"] == {"rncfg": str(build / "rncfg")}
    assert (got["name"], got["stands_for"], got["catalogue"]) == ("Sergey's", "nRF52840", "local")
    with pytest.raises(devices.DeviceError, match="build it first"):
        devices.resolve("broken", devices_dir=str(dd), arch=ARCH)
    rows = {r["ref"]: r for r in devices.listing(str(dd), ARCH)}
    assert rows["sergey"]["local"] and "error" in rows["broken"]


def test_a_path_resolves_to_a_package_or_a_workspace_build(tmp_path):
    dd = installed_devices(tmp_path)
    pkg = os.path.join(dd, "dev", "reticulous_hw-simesh-aarch64_20260920000000")
    assert devices.resolve(pkg, arch=ARCH)["stamp"] == "20260920000000"
    other = os.path.join(dd, "dev", "reticulous_hw-simesh-x86_64_20260930000000")
    with pytest.raises(devices.DeviceError, match="built for x86_64"):
        devices.resolve(other, arch=ARCH)

    build = tmp_path / "ws" / "reticulous" / "esp-idf" / "build.linux"
    (build / "data_merged").mkdir(parents=True)
    (build / "reticulous.elf").write_text("elf")
    base = tmp_path / "ws" / "SIMesh" / "testbed"
    base.mkdir(parents=True)
    got = devices.resolve("../../reticulous/esp-idf/build.linux", base_dir=str(base), arch=ARCH)
    assert got["elf"] == str(build / "reticulous.elf")
    assert got["fixed"] == str(build / "data_merged")
    assert got["kind_type"] == "reticulous" and got["node"] is None
    assert len(got["stamp"]) == 14 and got["stamp"].isdigit()
    with pytest.raises(devices.DeviceError, match="neither"):
        devices.resolve(str(base), arch=ARCH)


def test_the_cli_resolves_as_json(tmp_path, capsys):
    build = tmp_path / "build.linux"
    build.mkdir()
    (build / "reticulous.elf").write_text("elf")
    assert devices.main(["resolve", str(build)]) == 0
    assert '"kind_type": "reticulous"' in capsys.readouterr().out
    assert devices.main(["resolve", str(tmp_path / "missing")]) == 1
