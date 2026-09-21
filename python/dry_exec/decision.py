"""System-One decision layer adapters: receipt parsing and escalation routing.

The decision itself is computed by the Rust core from the scalar summary of the state delta; this
module maps the routed receipt onto the type-safe model and enforces escalation at the ergonomic
entry points. No scoring logic is duplicated here.
"""

from typing import Any, Dict, Optional
from dry_exec.exceptions import DecisionEscalationError
from dry_exec.models import DecisionReceipt, Noul


def parse_decision(raw: Optional[Dict[str, Any]]) -> Optional[DecisionReceipt]:
    """Maps the FFI decision payload onto the type-safe receipt model."""
    if not raw:
        return None
    return DecisionReceipt(
        choice=raw["choice"],
        risk_score=raw["risk_score"],
        noul_trigger=raw["noul_trigger"],
        reason=raw["reason"],
    )


def is_escalated(receipt: Optional[DecisionReceipt]) -> bool:
    """Whether the receipt requires explicit escalation approval before committing."""
    return receipt is not None and receipt.noul_trigger is Noul.ESCALATE


def enforce_escalation(
    receipt: Optional[DecisionReceipt], *, force: bool = False
) -> None:
    """Halts control flow unless the receipt routes to auto-commit or the commit is forced."""
    if not is_escalated(receipt) or force:
        return
    raise DecisionEscalationError(
        message=receipt.reason,
        risk_score=receipt.risk_score,
        reason=receipt.reason,
        choice=receipt.choice.value,
    )
