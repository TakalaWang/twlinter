import json
import re

def main():
    with open('assets/ruleset.json', 'r', encoding='utf-8') as f:
        lines = f.readlines()
    
    with open('assets/ruleset.json', 'r', encoding='utf-8') as f:
        data = json.load(f)

    rules = data.get('spelling_rules', [])
    
    # Regex for standard patterns
    # Long-form annotations include full cn/tw detail after 。; short-form
    # annotations like '@domain 醫學' or '@geo country (國名)' are also valid.
    # Only flag entries that lack even the minimal structure.
    geo_pattern = re.compile(r'^@geo\s+(country|city|landmark|university)\s+')
    domain_pattern = re.compile(r'^@domain\s+\S+')

    issues = []
    
    # helper to find line number of a rule based on its 'from' value
    def get_line(f_val, start_idx=0):
        search_str = f'"from": "{f_val}"'
        for i in range(start_idx, len(lines)):
            if search_str in lines[i]:
                return i + 1, i + 1
        return -1, start_idx
        
    search_idx = 0
    
    # We will track @geo targets to find duplicates
    geo_targets = {}

    for rule in rules:
        f_val = rule.get('from', '')
        t_vals = rule.get('to', [])
        ctx = rule.get('context', '')
        rtype = rule.get('type', '')

        line_num, search_idx = get_line(f_val, search_idx)
        loc = f"assets/ruleset.json:{line_num}"

        # 4. Missing annotations
        if rtype == 'cross_strait':
            if '@geo' not in ctx and '@domain' not in ctx and '@seealso' not in ctx and '@compound' not in ctx:
                issues.append(f"[WARN] {loc} missing annotation for cross_strait rule")

        # 3. Inconsistent format
        if '@geo' in ctx:
            if not geo_pattern.search(ctx):
                issues.append(f"[INFO] {loc} inconsistent format for @geo annotation")
            
            # 5. Duplicate geography rules
            for t in t_vals:
                if t in geo_targets:
                    prev_loc, prev_f = geo_targets[t]
                    if prev_f != f_val:
                        issues.append(f"[WARN] {loc} duplicate geography rule for '{t}' (also see {prev_loc} '{prev_f}')")
                else:
                    geo_targets[t] = (loc, f_val)
                    
        if '@domain' in ctx:
            if not domain_pattern.search(ctx):
                issues.append(f"[INFO] {loc} inconsistent format for @domain annotation")

        # 6. Suspicious 'to' values (ai_filler rules use to:[""] for deletion)
        is_deletion = rtype == 'ai_filler'
        for t in t_vals:
            if (t == '' and not is_deletion) or '?' in t or '\ufffd' in t:
                issues.append(f"[ERROR] {loc} suspicious 'to' value: '{t}'")

    # Output issues
    with open('auto_issues.txt', 'w', encoding='utf-8') as out:
        for iss in issues:
            out.write(iss + '\n')

    # Dump for semantic review
    with open('geo_review.txt', 'w', encoding='utf-8') as out:
        for rule in rules:
            if '@geo' in rule.get('context', ''):
                out.write(f"{rule.get('from', '')} -> {rule.get('to', [])} | {rule.get('context', '')}\n")

    with open('domain_review.txt', 'w', encoding='utf-8') as out:
        for rule in rules:
            if '@domain' in rule.get('context', ''):
                out.write(f"{rule.get('from', '')} -> {rule.get('to', [])} | {rule.get('context', '')}\n")

if __name__ == '__main__':
    main()
