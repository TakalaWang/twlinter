import json

def get_line(lines, search_str, start_idx=0):
    for i in range(start_idx, len(lines)):
        if search_str in lines[i]:
            return i + 1, i + 1
    return -1, start_idx

def check():
    with open('assets/ruleset.json', 'r', encoding='utf-8') as f:
        lines = f.readlines()
        
    with open('assets/ruleset.json', 'r', encoding='utf-8') as f:
        data = json.load(f)

    rules = data.get('spelling_rules', [])
    search_idx = 0
    geo_targets = {}
    
    for rule in rules:
        f_val = rule.get('from', '')
        t_vals = rule.get('to', [])
        ctx = rule.get('context', '')
        rtype = rule.get('type', '')

        search_str = f'"from": "{f_val}"'
        line_num, search_idx = get_line(lines, search_str, search_idx)
        loc = f"assets/ruleset.json:{line_num}"

        # Missing annotations
        if rtype == 'cross_strait':
            if (
                '@geo' not in ctx
                and '@domain' not in ctx
                and '@seealso' not in ctx
                and '@compound' not in ctx
                and '@person' not in ctx
            ):
                print(f"[WARN] {loc} missing annotation for cross_strait rule '{f_val}'")

        # Duplicate geography
        if '@geo' in ctx:
            for t in t_vals:
                if t in geo_targets:
                    prev_loc, prev_f = geo_targets[t]
                    print(f"[WARN] {loc} duplicate geography entity mapping to '{t}' (same as {prev_loc} '{prev_f}')")
                else:
                    geo_targets[t] = (loc, f_val)
                    
        # Suspicious to values
        for t in t_vals:
            if t == '' or '?' in t or '\ufffd' in t:
                print(f"[ERROR] {loc} suspicious 'to' value: '{t}' for '{f_val}'")

        # Misclassified domains
        misclassifications = {
            '空氣淨化器': '程式設計',
            '飛行模式': '程式設計',
            '頁眉': '程式設計',
            '頁腳': '程式設計',
            '零部件': '程式設計',
            '首選項': '程式設計',
            '連接器': '商業/金融',
            '編程語言': '通訊',
            '舉報': '電子/硬體',
            '矢量': '商業/金融',
            '進程管理': '通訊',
            '高性能計算': '通訊',
            '析構函數': '商業/金融',
            '構造函數': '商業/金融',
            '性價比': '通訊',
            '手動檔': '通訊',
            '地址空間': '通訊',
            '培訓': '程式設計',
            '地址欄': '通訊',
            '門戶網站': '電子/硬體',
            '引導程序': '',  # to_value empty check already catches it
            '黑客': ''     # to_value empty check already catches it
        }
        
        if f_val in misclassifications:
            expected_domain = misclassifications[f_val]
            if expected_domain and f'@domain {expected_domain}' in ctx:
                print(f"[ERROR] {loc} misclassified domain: '{f_val}' tagged as {expected_domain}")

if __name__ == '__main__':
    check()
