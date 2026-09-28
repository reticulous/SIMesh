# A pre: build script for a Portduino firmware built on a case-insensitive
# filesystem (a macOS volume shared into a Linux container). There,
# framework-portduino's api/String.h answers `#include <string.h>` in place of
# the C library's, and nothing declares strlen or memcpy. Platform-native
# works around this only when the host itself is macOS; this does the same on
# Linux: a directory holding a `string.h` that includes libc's by absolute
# path, ahead of everything else on the include path. The shim declares itself
# a system header so that libc's header, included from it, stays one, and
# GCC keeps its leniency towards redeclarations of libc functions. On a
# case-sensitive filesystem it does nothing.
import os

Import("env")

framework = env.PioPlatform().get_package_dir("framework-portduino")
api = os.path.join(framework or "", "ArduinoCore-API", "api")
libc = "/usr/include/string.h"
if framework and os.path.exists(os.path.join(api, "string.h")) and os.path.isfile(libc):
    shim = os.path.join(env.subst("$PROJECT_BUILD_DIR"), "casefold-shim")
    os.makedirs(shim, exist_ok=True)
    text = '#pragma once\n#pragma GCC system_header\n#include "%s"\n' % libc
    path = os.path.join(shim, "string.h")
    if not os.path.isfile(path) or open(path).read() != text:
        with open(path, "w") as f:
            f.write(text)
    env.Prepend(CPPPATH=[shim])
