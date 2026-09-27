"""Load benchmark acceptance budgets from the canonical suite manifest."""

from __future__ import annotations

import json
import pathlib


MANIFEST = pathlib.Path(__file__).resolve().parents[1] / "manifest.json"


class BudgetConfigurationError(RuntimeError):
    pass


def entrypoint_budgets(identifier: str) -> dict[str, int]:
    manifest = json.loads(MANIFEST.read_text())
    entrypoint = next(
        (entry for entry in manifest.get("entrypoints", []) if entry.get("id") == identifier),
        None,
    )
    if entrypoint is None:
        raise BudgetConfigurationError(f"unknown benchmark entrypoint {identifier!r}")
    budgets = entrypoint.get("budgets")
    if not isinstance(budgets, dict) or not budgets:
        raise BudgetConfigurationError(f"benchmark entrypoint {identifier!r} has no budgets")
    invalid = {
        name: value
        for name, value in budgets.items()
        if not isinstance(name, str)
        or not isinstance(value, int)
        or isinstance(value, bool)
        or value < 0
    }
    if invalid:
        raise BudgetConfigurationError(
            f"benchmark entrypoint {identifier!r} has invalid budgets: {invalid!r}"
        )
    return budgets
