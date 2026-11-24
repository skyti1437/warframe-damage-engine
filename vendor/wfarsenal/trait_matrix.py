"""Single-trait trials using the portable precompute's reference and element rules."""

from dataclasses import replace
from functools import lru_cache
import math

from . import optimizer
from .event_profiles import battery_profiles


WEAKPOINT_CODES = {"W", "Y"}
SINGLE_ELEMENTS = {
    "4": ("heat", "blast", "gas", "radiation"),
    "5": ("viral", "blast", "magnetic", "cold"),
    "6": ("viral", "corrosive", "gas", "toxin"),
    "7": ("corrosive", "magnetic", "radiation", "electricity"),
}
COMPOUND_ELEMENTS = dict(GA="gas", CO="corrosive", VI="viral", RA="radiation", BL="blast", MA="magnetic")
# IDs match the plugin's public trait vocabulary. D and K share the base
# damage input ID, with their individual engine codes kept on each row.
STAT_IDS = {
    "0": "crit_chance", "1": "crit_damage", "2": "status_chance", "3": "status_duration",
    "4": "heat_damage", "5": "cold_damage", "6": "toxin_damage", "7": "electric_damage",
    "8": "impact_damage", "9": "puncture_damage", "A": "slash_damage",
    "B": "initial_combo", "C": "damage_vs_corpus", "D": "melee_damage", "E": "slide_crit",
    "F": "reload_speed", "G": "damage_vs_grineer", "H": "zoom", "I": "damage_vs_infested",
    "J": "attack_speed", "K": "melee_damage", "L": "magazine_capacity", "M": "ammo_max",
    "N": "combo_duration", "O": "extra_combo_count", "P": "punch_through", "Q": "combo_gain_chance",
    "R": "fire_rate", "S": "multishot", "T": "range", "U": "heavy_attack_efficiency",
    "V": "projectile_speed", "W": "weakpoint_damage", "X": "finisher_damage",
    "Y": "weakpoint_crit_chance", "Z": "recoil", "SD": "status_damage", "AE": "ammo_efficiency",
    "HR": "reload_holstered", "FO": "damage_vs_orokin", "FT": "damage_vs_techrot",
    "FS": "damage_vs_scaldra", "HD": "melee_heavy_attack_damage", "HC": "melee_heavy_attack_charge",
    "SL": "melee_slam_damage", "PA": "melee_parry_angle",
    **{code: f"{element}_damage" for code, element in COMPOUND_ELEMENTS.items()},
}
UNITS = {"percent": "%", "x": "x"}
FLAT_UNITS = {"P": "m", "T": "m", "N": "s", "B": "", "PA": "°"}
RESERVE_AMMO = {
    "/Lotus/Weapons/Corpus/Bow/Longbow/CrpBow": 6,
    "/Lotus/Weapons/Grineer/Bows/GrnBow/GrnBowWeapon": 5,
    "/Lotus/Weapons/Grineer/KuvaLich/LongGuns/Zarr/KuvaZarr": 5,
    "/Lotus/Weapons/Grineer/KuvaLich/LongGuns/Ogris/KuvaOgris": 7,
    "/Lotus/Weapons/Grineer/KuvaLich/LongGuns/Tonkor/KuvaTonkor": 11,
    "/Lotus/Weapons/Infested/Bow/InfCernosBow/InfCernos": 72,
    "/Lotus/Weapons/Tenno/Bows/PrimeDerelictCernos/DerelictCernos": 7,
    "/Lotus/Weapons/Corpus/LongGuns/CrpBriefcaseLauncher/CrpBriefcaseLauncher": 16,
}
HEAVY_CALIBER = "/Lotus/Upgrades/Mods/Rifle/DualStat/CorruptedDamageRecoilRifle"
SERRATION = "/Lotus/Upgrades/Mods/Rifle/WeaponDamageAmountMod"


def candidate_sets(weapon, meta, templates):
    kind = optimizer._riven_type(meta["rivenType"], weapon)
    disposition = meta.get("disposition")
    if not disposition or kind not in optimizer.RIVEN_BASE_VALUES:
        return ({}, {}), ({}, {})
    bases = {**optimizer.RIVEN_BASE_VALUES[kind], **optimizer._template_bases(templates, kind)}
    if kind == "melee":
        bases["PA"] = 81  # The template stores parry angle on a percentage scale.
    fusion = optimizer._fusion_codes_from_templates(templates, kind)
    allowed = [optimizer._riven_allowed_codes(templates, kind, weapon, negative=flag)
               for flag in (False, True)]
    groups = []
    for count in (3, 2):
        signs = []
        for negative, expected in zip((False, True), allowed, strict=True):
            lines, _ = optimizer._candidate_lines(
                kind, disposition, (), negative=negative, bases=bases,
                available_fusion_codes=fusion, allowed_codes=expected,
                structure_scale=optimizer.theoretical_structure_scale(count, True, negative=negative)
                / optimizer.RIVEN_ROLL_HIGH,
            )
            candidates = {line.code: line for line in lines}
            if candidates.keys() != expected:
                raise optimizer.SimulationError(f"incomplete legal trait pool: {sorted(expected - candidates.keys())}")
            signs.append(candidates)
        groups.append(tuple(signs))
    return tuple(groups)


def element_search_pool(candidates, cards):
    # Keep the reference card families and the two strongest cards per element.
    families = {optimizer._mod_family(card) for card in cards}
    keys = {card.key for card in cards}
    for element in sorted(optimizer.BASE_ELEMENTS):
        ranked = sorted(
            ((sum(effect.value for effect in card.effects if effect.damage_type == element), card)
             for card in candidates),
            key=lambda item: (-item[0], item[1].drain, item[1].key),
        )
        keys.update(card.key for amount, card in ranked[:2] if amount > 0)
    return [card for card in candidates
            if card.required or card.key in keys or optimizer._mod_family(card) in families]


def _reserve_ammo(weapon, meta):
    path = meta["path"]
    if meta["weaponType"] in {"melee", "zaw"} or path in battery_profiles():
        return None
    for key in ("reserveAmmo", "reserve", "ammoCapacity", "maxAmmo", "AmmoMax"):
        value = weapon.get(key)
        if type(value) is int and value > 0:
            return value
    return RESERVE_AMMO.get(path)


def _reference_replay(ctx, result, mod_catalog, mechanics, evaluate):
    """Replay the Forma layout and its ablations through the shared event scorer."""
    weapon = ctx["weapon"]
    evaluation = ctx["evaluation"]
    cards, stance, exilus = evaluation.cards, result["stance"], result["exilus"]

    def layout(cards):
        return optimizer._forma_summary(
            weapon, cards, stance=stance, exilus=exilus, include_placement=True,
        )

    def replay(cards):
        evaluation = optimizer._evaluate(weapon, cards, None, **evaluate)
        return evaluation, optimizer._event_window(evaluation.stats, ctx["benchmarkContext"])

    plan = layout(cards)
    if not plan["fits"]:
        raise optimizer.SimulationError("reference build does not fit its capacity")
    placements = [(slot, row) for slot, row in enumerate(plan["placement"]) if row is not None]
    cards = tuple(cards[row["origin"]] for _, row in placements)
    baseline, window = replay(cards)
    used = plan["drain"]
    replaced_slot = None

    # Match the precompute's same-slot Serration substitution when its DPS is equal.
    serration = next((card for card in ctx["candidates"] if card.key == SERRATION and card.rank == 10), None)
    if serration is not None and all(card.key != SERRATION for card in cards):
        for i, card in enumerate(cards):
            if card.key != HEAVY_CALIBER or card.rank != 10:
                continue
            slot, placement = placements[i]
            new_drain = optimizer._slot_drain(serration.drain, serration.polarity, placement["polarity"])
            if new_drain < placement["drain"]:
                trial_cards = (*cards[:i], serration, *cards[i + 1:])
                trial, alt_window = replay(trial_cards)
                if math.isclose(window["windowDps"], alt_window["windowDps"], rel_tol=1e-12):
                    used += new_drain - placement["drain"]
                    placements[i] = (slot, {**placement, "drain": new_drain})
                    cards, baseline, window = trial_cards, trial, alt_window
                    replaced_slot = slot
            break

    mods, without, unmodeled = [], {}, []
    for i, (card, (slot, placement)) in enumerate(zip(cards, placements, strict=True)):
        meta = mod_catalog[card.record["path"]]
        mods.append(dict(
            path=meta["path"], nameEn=meta["nameEn"], nameZh=meta["nameZh"],
            slot=slot, rank=card.rank, polarity=card.polarity,
            slotPolarity=placement["polarity"], drain=placement["drain"],
        ))
        if mechanics.get(meta["path"], {}).get("status") in {"event-unmodeled", "unsupported"}:
            unmodeled.append(slot)
            continue
        _, dropped = replay(cards[:i] + cards[i + 1:])
        without[str(slot)] = round(dropped["windowDps"], 2)

    arcanes = [
        dict(path=record["path"], rank=rank, nameEn=record.get("nameEn"), nameZh=record.get("nameZh"),
             **{key: raw[key] for key in ("stacks", "uptime") if key in raw})
        for record, rank, raw in getattr(ctx["passives"], "event_arcanes", ())
    ]
    build = dict(
        mods=mods, arcanes=arcanes, exilus=exilus, stance=stance,
        incarnonChoices=result["incarnonChoices"], innateElement=result["innateElement"],
        full_dps=round(window["windowDps"], 2), kpm=round(window["killsPerMinute"], 4),
        without=without, unmodeledSlots=unmodeled, serrationSlot=replaced_slot,
        capacity=dict(used=used, limit=plan["limit"], forma=plan["total"], valid=True),
    )
    return build, baseline


def _element_search_options(ctx, special_cards, special_cost):
    passives = ctx["passives"]
    if special_cards:
        holder = passives or optimizer._Card("__passives__", None, 0, (), (), 0, None)
        passives = replace(holder, special_cards=(*holder.special_cards, *special_cards))
    return dict(
        scenario=ctx["scenario"], metric=ctx["metric"], passives=passives,
        capacity_limit=ctx["capacityLimit"] - special_cost,
        target_context=ctx["benchmarkContext"],
    )


def _element_reoptimized_score(weapon, pool, seeds, riven, slot_count, options):
    seed = max(
        (trial for cards in seeds
         if (trial := optimizer._evaluate(weapon, cards, riven, slot_limit=slot_count, **options))),
        key=lambda trial: trial.score, default=None,
    )
    optimized = optimizer.optimize_for_element(
        weapon, pool, riven, slot_count=slot_count, profile="fast", beam_width=1, shortlist=8,
        seed_cards=seed.cards if seed else None, **options,
    )
    return max(optimized.score, seed.score) if seed else optimized.score


def _element_trial_gain(weapon, baseline, riven, damage_type, options):
    cards = baseline.cards
    damage = {key: value for key, value in baseline.stats.damage_types.items() if value > 0}
    evaluate = {
        **options,
        "capacity_limit": options["capacity_limit"] + optimizer._capacity((riven,)),
        "slot_limit": len(cards) + 1,
        "reorder_elements": False,
        "required_damage_type": damage_type,
    }
    best = None
    for position in range(len(cards) + 1):
        trial = optimizer._evaluate(weapon, cards, riven, fixed_riven_index=position, **evaluate)
        if trial is None:
            continue
        trial_damage = trial.stats.damage_types
        if ({key for key, value in trial_damage.items() if value > 0} == damage.keys()
                and trial_damage[damage_type] > damage[damage_type]):
            best = trial.score if best is None else max(best, trial.score)
    return None if best is None else optimizer._percent_gain(best, baseline.score)


def _apply_element_baselines(ctx, rows, groups, options):
    weapon = ctx["weapon"]

    @lru_cache(maxsize=None)
    def baseline(element):
        damage_type = optimizer._ELEMENT_TARGET_TYPES[element]
        try:
            trial = optimizer.optimize_for_element(
                weapon, ctx["candidates"], riven=None, required_damage_type=damage_type,
                profile="fast", slot_count=ctx["slotCount"], **options,
            )
        except optimizer.SimulationError as exc:
            if str(exc) != f"no capacity-valid build satisfies damage type {damage_type}":
                raise
            return None
        return trial if trial.score > 0 else None

    for row in rows:
        code = row["code"]
        compound = code in COMPOUND_ELEMENTS
        if not row["modeled"] or (not compound and code not in SINGLE_ELEMENTS):
            continue
        default_gains = (row["pct_31"], row["pct_21"])
        negative = any(gain is not None and gain < -0.05 for gain in default_gains)
        if not negative and (compound or any(gain is not None and gain > 0 for gain in default_gains)):
            continue
        elements = (COMPOUND_ELEMENTS[code],) if compound else SINGLE_ELEMENTS[code]
        candidates = [(element, trial) for element in elements if (trial := baseline(element)) is not None]
        rivens = [optimizer._card_from_lines([positive[code]]) for positive, _ in groups]
        for element, trial in sorted(candidates, key=lambda pair: pair[1].score, reverse=True):
            damage_type = optimizer._ELEMENT_TARGET_TYPES[element]
            gains = [_element_trial_gain(weapon, trial, riven, damage_type, options) for riven in rivens]
            if all(gain is None for gain in gains):
                continue
            accepted = []
            for roll, gain, default_gain in zip(("31", "21"), gains, default_gains, strict=True):
                if compound and (default_gain is None or default_gain >= -0.05):
                    continue
                if gain is not None and gain >= -0.05:
                    row[f"pct_{roll}"] = round(gain, 2)
                    accepted.append(int(roll))
            if accepted:
                row["elem_build"] = "electric" if element == "electricity" else element
                if len(accepted) == 1:
                    row["elem_build_roll"] = accepted[0]
            break


def calculate(ctx, result, templates, mod_catalog, mechanics):
    """Return the fixed reference loadout and both complete legal trait tables."""
    weapon, meta = ctx["weapon"], ctx["catalogWeapon"]
    groups = candidate_sets(weapon, meta, templates)
    special_cards = (ctx["exilusCard"],) if ctx["exilusCard"] is not None else ()
    special_cost = int((result["exilus"] or {}).get("drain") or 0)
    evaluate = dict(
        scenario=ctx["scenario"], metric=ctx["metric"], passives=ctx["passives"],
        slot_limit=len(ctx["evaluation"].cards) + 1, capacity_limit=999,
        target_context=ctx["benchmarkContext"], reorder_elements=False,
        special_cards=special_cards, special_capacity_cost=special_cost,
    )
    build, baseline = _reference_replay(ctx, result, mod_catalog, mechanics, evaluate)
    cards = baseline.cards
    weakpoint_options = {
        **evaluate,
        "target_context": {**evaluate["target_context"], "headshotChance": 1.0, "hit_location": "head"},
    }
    weakpoint = not evaluate["target_context"]["headshotChance"] and bool(WEAKPOINT_CODES & groups[0][0].keys())
    baseline_wp = optimizer._evaluate(weapon, cards, None, **weakpoint_options) if weakpoint else None
    reserve = _reserve_ammo(weapon, meta)
    mode = next((row for row in meta["fireModes"] if row["index"] == result["fireMode"]), {})
    is_form = bool(mode.get("isIncarnonForm"))
    cycle = is_form and ctx["scenario"].incarnon_cycle
    matrix = dict(
        baseline_kpm=round(baseline.score, 4), baseline_dps=build["full_dps"],
        baseline_weakpoint_kpm=round(baseline_wp.score, 4) if baseline_wp else None,
        reserve_ammo=reserve, low_ammo=reserve is not None and reserve <= 30,
        incarnon_form=is_form and not cycle, incarnon_cycle=cycle,
    )
    pool = element_search_pool(ctx["candidates"], cards)
    least_impact = sorted(
        range(len(cards)), key=lambda i: -build["without"].get(str(build["mods"][i]["slot"]), -1),
    )[:2]
    slots = ctx["slotCount"]
    seeds = [cards] if len(cards) < slots else [
        cards[:i] + cards[i + 1:] for i, card in enumerate(cards)
        if i in least_impact or any(effect.damage_type in optimizer.BASE_ELEMENTS for effect in card.effects)
    ]
    element_options = _element_search_options(ctx, special_cards, special_cost)
    neutral = optimizer._neutral_negative_codes(weapon)
    for i, sign in enumerate(("positive", "negative")):
        rows = matrix[sign] = []
        for code, line31 in sorted(groups[0][i].items()):
            line21 = groups[1][i][code]
            modeled = bool(line31.effect and line31.effect.supported)
            if modeled != bool(line21.effect and line21.effect.supported):
                raise optimizer.SimulationError(f"inconsistent trait model for {code}")
            row = dict(
                code=code, stat_id=STAT_IDS[code],
                unit=FLAT_UNITS[code] if line31.unit == "flat" else UNITS[line31.unit],
                spliced=code in optimizer._FUSION_LINE_CODES, modeled=modeled,
                neutral=sign == "negative" and code in neutral,
                weakpoint=modeled and sign == "positive" and weakpoint and code in WEAKPOINT_CODES,
                reoptimized=modeled and sign == "positive" and code in SINGLE_ELEMENTS,
                elem_build=None,
            )
            options, reference = (weakpoint_options, baseline_wp) if row["weakpoint"] else (evaluate, baseline)
            for roll, line in (("31", line31), ("21", line21)):
                row[f"shown_{roll}"] = round(-line.value if line.negative else line.value, 2)
                pct = None
                if modeled:
                    riven = optimizer._card_from_lines([line])
                    trial = optimizer._evaluate(weapon, cards, riven, fixed_riven_index=len(cards), **options)
                    score = trial.score
                    if row["reoptimized"]:
                        score = max(score, _element_reoptimized_score(weapon, pool, seeds, riven, slots, element_options))
                    if reference.score > 0:
                        pct = round(optimizer._percent_gain(score, reference.score), 2)
                row[f"pct_{roll}"] = pct
            rows.append(row)
    _apply_element_baselines(ctx, matrix["positive"], groups, element_options)
    return build, matrix
