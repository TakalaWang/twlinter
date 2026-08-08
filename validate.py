import json
import unicodedata
from collections import defaultdict
import re

def main():
    with open('assets/ruleset.json', 'r', encoding='utf-8') as f:
        data = json.load(f)
    
    rules = data.get('spelling_rules', [])
    
    print("=== A. Data Integrity ===")
    for i, r in enumerate(rules):
        f_val = r.get('from', '')
        t_val = r.get('to', [])
        if not f_val:
            print(f"[ERROR] Rule {i} has empty 'from'.")
        if t_val and f_val == t_val[0]:
            print(f"[ERROR] Rule '{f_val}': from == to[0]")
        if f_val in t_val:
            print(f"[ERROR] Rule '{f_val}': 'from' is in 'to' array")

    print("\n=== B. @domain_review.txt accuracy (30 spot checks) ===")
    domain_rules = [r for r in rules if '@domain' in r.get('context', '')]
    # To be deterministic and cover a spread, let's take every Nth rule
    if domain_rules:
        step = max(1, len(domain_rules) // 30)
        spot_checks = domain_rules[::step][:30]
        for r in spot_checks:
            print(f"[CHECK] {r.get('from', '')} -> {r.get('to', [])} | Context: {r.get('context', '')}")

    print("\n=== C. @geo_review.txt accuracy ===")
    geo_rules = [r for r in rules if '@geo' in r.get('context', '')]
    print(f"Total @geo rules found: {len(geo_rules)}")
    for r in geo_rules:
        print(f"[GEO] {r.get('from', '')} -> {r.get('to', [])} | Context: {r.get('context', '')}")
    
    # Check for missing geo tags in common countries (basic check)
    countries = ['中國', '美國', '日本', '德國', '法國', '英國']
    for r in rules:
        if '@geo' not in r.get('context', ''):
            f_val = r.get('from', '')
            if any(c in f_val for c in countries):
                print(f"[WARN] Potential missing @geo tag: {f_val}")

    print("\n=== D. Context Quality (short contexts without disambiguation) ===")
    for r in rules:
        ctx = r.get('context', '')
        if '@domain' in ctx:
            # Check if it's strictly just the domain tag
            clean_ctx = re.sub(r'@domain\s+\S+[。，]?', '', ctx).strip()
            if len(clean_ctx) < 2:
                print(f"[WARN] Short context for '{r.get('from', '')}': {ctx}")

    print("\n=== E. Duplicate Detection ===")
    from_seen = defaultdict(list)
    norm_seen = defaultdict(list)
    for r in rules:
        f_val = r.get('from', '')
        from_seen[f_val].append(r)
        
        # Near duplicate: remove spaces and normalize
        norm_val = unicodedata.normalize('NFKC', f_val).replace(' ', '').lower()
        norm_seen[norm_val].append(r)
        
    for f_val, items in from_seen.items():
        if len(items) > 1:
            print(f"[ERROR] Exact duplicate 'from' value: '{f_val}' ({len(items)} times)")
            
    for norm_val, items in norm_seen.items():
        if len(items) > 1:
            distinct_froms = set(i.get('from', '') for i in items)
            if len(distinct_froms) > 1:
                print(f"[WARN] Near-duplicate detected for normalized '{norm_val}': {distinct_froms}")

    print("\n=== F. New rule quality ===")
    # Let's see if we can identify the new rules. The prompt says 122 new rules.
    # Maybe we just list the rules with context_clues to sample them, or look at the last 122 rules.
    new_rules = rules[-122:] # Assuming appended at the end
    print(f"Checking last 122 rules (assuming they are the new ones):")
    for r in new_rules[:15]: # Print first 15 to review
        print(f"[NEW] {r.get('from', '')} -> {r.get('to', [])} | Context: {r.get('context', '')} | Clues: {r.get('context_clues', [])}")
        
if __name__ == "__main__":
    main()
