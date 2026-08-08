import json
import re

def check():
    with open('assets/ruleset.json', 'r', encoding='utf-8') as f:
        data = json.load(f)

    rules = data.get('spelling_rules', [])
    geo_pattern = re.compile(r'^@geo\s+(country|city|landmark|university)\s+\([^)]+\)。cn「[^」]+」；tw「[^」]+」')
    domain_pattern = re.compile(r'^@domain\s+[^。]+。.*')

    seen_geo = {}
    for i, rule in enumerate(rules):
        rule_num = i + 1
        f = rule.get('from', '')
        t = rule.get('to', [])
        ctx = rule.get('context', '')
        rtype = rule.get('type', '')

        # Missing annotations
        if rtype == 'cross_strait':
            if '@geo' not in ctx and '@domain' not in ctx and '@seealso' not in ctx and '@compound' not in ctx:
                print(f"[WARN] file:{rule_num} missing annotation for cross_strait: '{f}' ctx: '{ctx}'")

        # Formatting
        if '@geo' in ctx:
            if not geo_pattern.search(ctx):
                print(f"[INFO] file:{rule_num} inconsistent format for @geo: '{f}' ctx: '{ctx}'")
            
            # Duplicates
            # extract entity name roughly
            match = re.search(r'@geo\s+(?:country|city|landmark|university)\s+\(([^)]+)\)', ctx)
            if match:
                entity = match.group(1)
                if entity in seen_geo:
                    print(f"[WARN] file:{rule_num} duplicate geography entity '{entity}' (prev: {seen_geo[entity]})")
                seen_geo[entity] = f
        
        if '@domain' in ctx:
            if not domain_pattern.search(ctx):
                print(f"[INFO] file:{rule_num} inconsistent format for @domain: '{f}' ctx: '{ctx}'")
        
        # Suspicious 'to' values
        for to_val in t:
            if to_val == '' or '?' in to_val or len(to_val) > 20:
                print(f"[WARN] file:{rule_num} suspicious to value: '{to_val}'")

if __name__ == '__main__':
    check()
