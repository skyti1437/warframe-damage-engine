"""Compatibility for the published Portable analysis statics."""

# The full runtime digest includes the native source identity. The audited
# update changes the default interactive clock and adds explicit loadout
# contexts. Published statics were already baked without a deadline and do
# not use the new explicit heavy, starting-combo or trait-matrix requests.
_COMPATIBLE_RUNTIME_DIGESTS = {
    "f5874c328c26a54897222d3637c09b298e54f99e": "535ed49f5a51149d",
    # The opt-in Incarnon gauge override has its own scenario cache key.
    "b88371809169575fc2a3fce266e669c9c89c12b2": "535ed49f5a51149d",
    # Burston EVO II now defaults to 2-1 on tied scores.
    "b14f87bc35554a456877b1edabfd6cd8cb051d40": "535ed49f5a51149d",
}


def portable_score_fingerprint(runtime_digest: str) -> str:
    return _COMPATIBLE_RUNTIME_DIGESTS.get(runtime_digest, runtime_digest[:16])
