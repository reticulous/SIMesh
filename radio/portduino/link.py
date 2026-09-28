# The chip library's headers for this library's source, and the library
# itself (build/libsimradio.a, from `simesh build radio`) for the program.
import inspect
import os

Import("env")

here = os.path.dirname(os.path.realpath(inspect.getframeinfo(inspect.currentframe()).filename))
radio = os.path.dirname(here)
env.Append(CPPPATH=[os.path.join(radio, "include")])

program = DefaultEnvironment()
# By path: build/ holds the shared library too, which -lsimradio would pick.
program.Append(LIBS=[program.File(os.path.join(radio, "build", "libsimradio.a")), "pthread", "dl"])
