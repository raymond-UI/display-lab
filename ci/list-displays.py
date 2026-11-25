#!/usr/bin/env python3
"""Print every display CoreGraphics knows about: online and active.

Diagnostic for CI. A virtual display can exist without being in the active
list (mirrored, asleep, or never brought up), and the difference matters.
"""
import ctypes
import ctypes.util

cg = ctypes.CDLL(ctypes.util.find_library("CoreGraphics"))
u32 = ctypes.c_uint32


class CGPoint(ctypes.Structure):
    _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]


class CGSize(ctypes.Structure):
    _fields_ = [("w", ctypes.c_double), ("h", ctypes.c_double)]


class CGRect(ctypes.Structure):
    _fields_ = [("origin", CGPoint), ("size", CGSize)]


cg.CGDisplayBounds.restype = CGRect
for name in ("CGDisplayIsActive", "CGDisplayIsOnline", "CGDisplayIsAsleep", "CGDisplayIsInMirrorSet",
             "CGDisplayIsMain", "CGDisplayIsBuiltin", "CGDisplayVendorNumber", "CGDisplayModelNumber",
             "CGDisplaySerialNumber", "CGDisplayMirrorsDisplay"):
    getattr(cg, name).restype = u32
    getattr(cg, name).argtypes = [u32]


def listing(fn):
    ids = (u32 * 32)()
    count = u32(0)
    err = fn(32, ids, ctypes.byref(count))
    return err, list(ids[: count.value])


for label, fn in (("online", cg.CGGetOnlineDisplayList), ("active", cg.CGGetActiveDisplayList)):
    err, ids = listing(fn)
    print(f"{label:9} err={err} ids={ids}")

_, online = listing(cg.CGGetOnlineDisplayList)
for d in online:
    b = cg.CGDisplayBounds(d)
    print(
        f"display {d}: vendor={cg.CGDisplayVendorNumber(d)} model={cg.CGDisplayModelNumber(d)} "
        f"serial={cg.CGDisplaySerialNumber(d)} bounds={b.size.w:.0f}x{b.size.h:.0f}@({b.origin.x:.0f},{b.origin.y:.0f}) "
        f"active={cg.CGDisplayIsActive(d)} asleep={cg.CGDisplayIsAsleep(d)} main={cg.CGDisplayIsMain(d)} "
        f"mirrored={cg.CGDisplayIsInMirrorSet(d)} mirrors={cg.CGDisplayMirrorsDisplay(d)}"
    )
