"""Antennas: the catalogue, the pattern, and a pair's gain in three dimensions."""

import os
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import antennas  # noqa: E402
import store  # noqa: E402


def test_every_antenna_has_its_figures_a_description_and_a_picture():
    listed = antennas.listing()
    assert len(listed) == 14 and antennas.DEFAULT_TYPE in antennas.catalogue()
    for a in listed:
        assert a["description"] and a["label"] and a["svg"].startswith("<svg"), a["type"]
        assert (a["hbw_deg"] is not None) == (a["kind"] == "directional"), a["type"]


def test_a_pattern_is_its_peak_on_the_lobe_3_db_down_at_half_a_beamwidth_and_floored():
    collinear = {"type": "fiberglass_collinear"}
    spec = antennas.catalogue()["fiberglass_collinear"]
    assert antennas.gain(collinear, 123, 0) == pytest.approx(spec["peak_dbi"])
    assert antennas.gain(collinear, 0, spec["vbw_deg"] / 2) == pytest.approx(spec["peak_dbi"] - 3)
    assert antennas.gain(collinear, 0, 90) == pytest.approx(spec["peak_dbi"] - spec["floor_db"])
    yagi = {"type": "yagi_directional", "azimuth_deg": 90, "elevation_deg": 10}
    spec = antennas.catalogue()["yagi_directional"]
    assert antennas.gain(yagi, 90, 10) == pytest.approx(spec["peak_dbi"])
    assert antennas.gain(yagi, 90 + spec["hbw_deg"] / 2, 10) == pytest.approx(spec["peak_dbi"] - 3)
    assert antennas.gain(yagi, 270, 10) == pytest.approx(spec["peak_dbi"] - spec["floor_db"])
    # An omni's aim is nothing: it is the same all round.
    whip = {"type": "whip_sma_quarter_wave", "azimuth_deg": 90}
    assert antennas.gain(whip, 0, 0) == antennas.gain(whip, 200, 0)


def test_an_antenna_is_checked_and_a_directional_one_aimed():
    assert antennas.check(None, "n") == {"type": antennas.DEFAULT_TYPE}
    assert antennas.check({"type": "panel_directional", "azimuth_deg": -90}, "n") == \
        {"type": "panel_directional", "azimuth_deg": 270.0, "elevation_deg": 0.0}
    assert antennas.check({"type": "rubber_duck", "azimuth_deg": 5}, "n") == {"type": "rubber_duck"}
    with pytest.raises(store.StoreError, match="no antenna of type"):
        antennas.check({"type": "dish"}, "n")


def test_the_direction_between_two_antennas_is_in_three_dimensions():
    az, el = antennas.direction((0, 0), 10.0, (1000, 0), 10.0)
    assert az == pytest.approx(90.0) and el == pytest.approx(0.0, abs=0.01)
    az, el = antennas.direction((0, 0), 110.0, (0, -1000), 10.0)
    assert az == pytest.approx(180.0) and el == pytest.approx(-5.71, abs=0.01)
    # At 40 km the far end has dropped below a level line by the curvature.
    _, el = antennas.direction((0, 0), 10.0, (40_000, 0), 10.0)
    assert el < -0.1
    # A tall collinear's narrow beam passes over a node close under it.
    high = {"xy": (0, 0), "top": 110.0, "antenna": {"type": "fiberglass_collinear"}}
    near = {"xy": (0, 300), "top": 2.0, "antenna": {"type": "rubber_duck"}}
    far = {"xy": (0, 5000), "top": 2.0, "antenna": {"type": "rubber_duck"}}
    assert antennas.pair_gain(high, near) < antennas.pair_gain(high, far) - 10
    assert antennas.pair_gain(high, far) == pytest.approx(antennas.pair_gain(far, high))
