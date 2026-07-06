"""Built-in case catalog.

Importing this module registers all shipped cases via ``@cluu_case``.
The package ``__init__`` imports it, so both the CLI and pytest see
the same registrations.

Add new built-in cases here. Out-of-tree cases can be registered by
importing ``cluu_harness`` and calling ``registry.register(Case(...))``
or using the ``@cluu_case`` decorator from their own module.
"""

from __future__ import annotations

from cluu_harness.cases import cluu_case


@cluu_case(
    "l2_login",
    marker_mode="l2_login",
    description="interactive login → session-procmgr spawn",
    tags=["login", "session"],
)
class L2Login:
    pass


@cluu_case(
    "l2_ls",
    marker_mode="l2_ls",
    description="basic ls of /etc",
    tags=["shell", "ls"],
)
class L2Ls:
    pass


@cluu_case(
    "l2_cd",
    marker_mode="l2_cd",
    description="cd/pwd shell builtins",
    tags=["shell", "cd"],
)
class L2Cd:
    pass


@cluu_case(
    "l2_mkdir",
    marker_mode="l2_mkdir",
    description="mkdir + mkdir -p",
    tags=["shell", "mkdir"],
)
class L2Mkdir:
    pass


@cluu_case(
    "l2_cluuterm_login",
    marker_mode="l2_cluuterm_login",
    description="inject credentials → procmgr SESSION_CREATE",
    tags=["login", "cluuterm"],
)
class L2CluutermLogin:
    pass


@cluu_case(
    "l2_cluuterm_exit",
    marker_mode="l2_cluuterm_exit",
    description="exit → cluuterm shutdown + compositor window destroyed",
    tags=["cluuterm", "compositor"],
)
class L2CluutermExit:
    pass


@cluu_case(
    "l2_vt4_default",
    marker_mode="l2_vt4_default",
    description="boot → compositor pinned to VT4",
    tags=["compositor", "boot"],
)
class L2Vt4Default:
    pass


@cluu_case(
    "l2_dev_nodes",
    marker_mode="l2_dev_nodes",
    description="ls /dev regression — dynamic /dev enumeration",
    tags=["vfs", "dev"],
)
class L2DevNodes:
    pass


@cluu_case(
    "m1_recv",
    marker_mode="m1_recv",
    description="recv/wakeup churn checks",
    tags=["recv", "ipc"],
)
class M1Recv:
    pass


@cluu_case(
    "m5_fairness",
    marker_mode="m5_fairness",
    description="mixed-load fairness/latency telemetry SLO checks",
    tags=["ipc", "slo"],
)
class M5Fairness:
    pass


@cluu_case(
    "b_spawn_warm",
    marker_mode="b_spawn_warm",
    description="spawn warm-cache benchmark + noop p95 SLO checks",
    tags=["spawn", "bench", "slo"],
)
class BSpawnWarm:
    pass


@cluu_case(
    "c_futex",
    marker_mode="c_futex",
    description="futex invoke wait/wake/timeout smoke",
    tags=["futex", "c-program"],
)
class CFutex:
    pass


@cluu_case(
    "l2_ext2write",
    marker_mode="l2_ext2write",
    description="end-to-end ext2 write smoke",
    tags=["ext2", "vfs"],
)
class L2Ext2Write:
    pass


__all__ = [
    "BSpawnWarm",
    "CFutex",
    "L2Cd",
    "L2CluutermExit",
    "L2CluutermLogin",
    "L2DevNodes",
    "L2Ext2Write",
    "L2Login",
    "L2Ls",
    "L2Mkdir",
    "L2Vt4Default",
    "M1Recv",
    "M5Fairness",
]
