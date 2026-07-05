"""Case declaration: dataclass + decorator registry.

The gen2 way to declare a test case:

.. code-block:: python

    from cluu_harness import Case, cluu_case

    @cluu_case("l2_my_probe", marker_mode="l2_my_probe",
    #            test_command="myprobe", run_wait_s=30)
    class MyProbe(Case):
        pass

Or programmatically:

.. code-block:: python

    case = Case(name="l2_my_probe", marker_mode="l2_my_probe",
                test_command="myprobe")
    registry.register(case)

The decorator and the dataclass are equivalent — pick whichever reads
best at the call site.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import ClassVar


@dataclass
class Case:
    """One harness case.

    Fields mirror the bash env-var surface. Anything left ``None`` falls
    back to ``case_defaults.get_defaults(marker_mode)`` at run time.
    """

    name: str
    marker_mode: str = "none"
    # None → derive from case_defaults; empty string → no command.
    test_command: str | None = None
    test_command_repeat: int = 1
    # Raw sendkey sequence (newline-separated in bash; list[str] here).
    # Each entry is "sendkey <name>", "mouse_move <dx> <dy>",
    # "mouse_button <n>", or "sleep <n>".
    sendkey_sequence: list[str] = field(default_factory=list)
    sendkey_sequence_nowait: bool | None = None
    post_sendkey: str | None = None
    run_wait_s: int | None = None
    shell_ready_wait_s: int | None = None
    # Extra KEYSTROKE_COMMANDS (typed via the HU translator).
    keystroke_commands: list[str] = field(default_factory=list)
    # Build policy: "full" (build if needed), "no_build" (reuse artifacts).
    build_mode: str = "full"
    # Expect a kernel fault (do not fail on PAGE_FAULT etc.).
    expect_fault: bool = False
    # Optional: forbid this fail marker (e.g. "mapfail: FAIL").
    fail_marker_override: str | None = None
    # Optional: override required markers completely.
    required_markers_override: list[str] | None = None
    # Free-form tags for filtering in pytest.
    tags: list[str] = field(default_factory=list)
    # Human-readable description (shown by --list).
    description: str = ""


class _Registry:
    """Singleton case registry. Access via the module-level ``registry``."""

    _cases: ClassVar[dict[str, Case]] = {}

    @classmethod
    def register(cls, case: Case) -> Case:
        if case.name in cls._cases:
            raise ValueError(f"duplicate case name {case.name!r}")
        cls._cases[case.name] = case
        return case

    @classmethod
    def get(cls, name: str) -> Case:
        return cls._cases[name]

    @classmethod
    def all_cases(cls) -> list[Case]:
        return list(cls._cases.values())

    @classmethod
    def names(cls) -> list[str]:
        return sorted(cls._cases)

    @classmethod
    def clear(cls) -> None:
        cls._cases.clear()


registry = _Registry


def cluu_case(
    name: str,
    *,
    marker_mode: str = "none",
    test_command: str | None = None,
    test_command_repeat: int = 1,
    sendkey_sequence: list[str] | None = None,
    sendkey_sequence_nowait: bool | None = None,
    post_sendkey: str | None = None,
    run_wait_s: int | None = None,
    shell_ready_wait_s: int | None = None,
    keystroke_commands: list[str] | None = None,
    build_mode: str = "full",
    expect_fault: bool = False,
    fail_marker_override: str | None = None,
    required_markers_override: list[str] | None = None,
    tags: list[str] | None = None,
    description: str = "",
) -> object:
    """Decorator: register a class as a harness case.

    The decorated class body is irrelevant — the decorator builds a
    :class:`Case` from the kwargs and registers it. Returning a decorator
    factory (not the class itself) keeps the call site self-documenting.
    """

    def deco(_cls: type) -> type:
        registry.register(
            Case(
                name=name,
                marker_mode=marker_mode,
                test_command=test_command,
                test_command_repeat=test_command_repeat,
                sendkey_sequence=sendkey_sequence or [],
                sendkey_sequence_nowait=sendkey_sequence_nowait,
                post_sendkey=post_sendkey,
                run_wait_s=run_wait_s,
                shell_ready_wait_s=shell_ready_wait_s,
                keystroke_commands=keystroke_commands or [],
                build_mode=build_mode,
                expect_fault=expect_fault,
                fail_marker_override=fail_marker_override,
                required_markers_override=required_markers_override,
                tags=tags or [],
                description=description,
            )
        )
        return _cls

    return deco


__all__ = ["Case", "cluu_case", "registry"]
