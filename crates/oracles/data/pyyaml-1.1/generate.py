#!/usr/bin/env python3
"""Regenerates scalars.tsv and documents.tsv: how PyYAML's YAML 1.1
resolver (`yaml.SafeLoader`) types plain scalars and loads a few documents
with merge keys. See SOURCE.md. Run from this directory:

    python3 generate.py
"""
import datetime
import json
import math

import yaml

SCALARS = [
    # booleans (yaml.org/type/bool)
    "yes", "Yes", "YES", "no", "No", "NO", "true", "True", "TRUE",
    "false", "False", "FALSE", "on", "On", "ON", "off", "Off", "OFF",
    "y", "Y", "n", "N", "yES", "tRUE",
    # nulls (yaml.org/type/null)
    "~", "null", "Null", "NULL", "nULL", "nil", "None",
    # decimal integers (yaml.org/type/int)
    "0", "1", "-1", "+1", "123", "-123", "+123", "1_000", "1__0", "1_",
    "9223372036854775807", "18446744073709551616", "-0",
    # octal
    "0644", "-0644", "+0644", "07", "08", "09", "010", "0_7", "00", "0o17",
    "-0o17", "0o8",
    # hexadecimal
    "0x1F", "0x1f", "-0x1F", "+0x1F", "0x_1F", "0x1_F", "0xG", "0X1F",
    # binary
    "0b1010", "-0b1010", "+0b1010", "0b1_0", "0b2", "0B1",
    # sexagesimal
    "190:20:30", "12:30", "1:30:00", "-1:30", "+1:30", "12:60", "0:30",
    "01:30", "1:5", "22:22", "1:30.5", "190:20:30.15", "1_0:30",
    # floats (yaml.org/type/float)
    "1.0", "-1.0", "+1.0", "1.", ".5", "-.5", "1.5e+3", "1.5e3", "1e3",
    "1E+3", "6.8523015e+5", "685.230_15e+03", "685_230.15", "1_0.5",
    ".inf", ".Inf", ".INF", "-.inf", "+.inf", ".nan", ".NaN", ".NAN",
    "inf", "nan", "1e400", "0.", "._", "1.2.3",
    # timestamps (yaml.org/type/timestamp)
    "2001-12-14", "2001-12-14t21:59:43.10-05:00",
    "2001-12-14 21:59:43.10 -5", "2002-12-14",
    # strings that look like something
    "=", "<<", "0x", "-", "+", ".", "1-2", "1:2:x", "a:b", "12:30:x",
]

DOCUMENTS = [
    ("merge-single", "base: &b {a: 1, b: 2}\nderived:\n  <<: *b\n  b: 3\n  c: 4\n"),
    ("merge-list", "x: &x {a: 1, b: 1}\ny: &y {b: 2, c: 2}\nz:\n  <<: [*x, *y]\n  c: 3\n"),
    ("merge-nested", "a: &a {k: 1}\nb: &b {<<: *a, l: 2}\nc: {<<: *b, m: 3}\n"),
    (
        "compose",
        "x-common: &common\n  restart: unless-stopped\n  environment:\n"
        "    TZ: UTC\nservices:\n  web:\n    <<: *common\n    image: nginx\n"
        "  api:\n    <<: *common\n    restart: always\n",
    ),
    ("config", "enabled: yes\nverbose: off\nmode: 0644\ntime: 12:30\nsize: 1_000\nmask: 0b1010\n"),
]


def typed(value):
    if value is None:
        return ("null", "~")
    if isinstance(value, bool):
        return ("bool", "true" if value else "false")
    if isinstance(value, int):
        return ("int", str(value))
    if isinstance(value, float):
        if math.isnan(value):
            return ("float", "nan")
        if math.isinf(value):
            return ("float", "inf" if value > 0 else "-inf")
        return ("float", repr(value))
    if isinstance(value, (datetime.date, datetime.datetime)):
        return ("timestamp", value.isoformat())
    return ("str", value)


def main():
    with open("scalars.tsv", "w", encoding="utf-8") as out:
        out.write(f"# PyYAML {yaml.__version__} SafeLoader; scalar\\ttype\\tvalue\n")
        for s in SCALARS:
            try:
                kind, value = typed(yaml.load("k: " + s, Loader=yaml.SafeLoader)["k"])
            except yaml.YAMLError:
                kind, value = ("error", "")
            out.write(f"{s}\t{kind}\t{value}\n")
    with open("documents.tsv", "w", encoding="utf-8") as out:
        out.write(f"# PyYAML {yaml.__version__} SafeLoader; name\\tjson(input)\\tjson(loaded, sorted keys)\n")
        for name, src in DOCUMENTS:
            loaded = yaml.load(src, Loader=yaml.SafeLoader)
            out.write(f"{name}\t{json.dumps(src)}\t{json.dumps(loaded, sort_keys=True)}\n")


if __name__ == "__main__":
    main()
