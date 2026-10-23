#!/usr/bin/env python3
"""人工筛选 expand_lexicon.py 的自动提取结果：
- misspellings：只保留"非真实词 → 正确词"的高精度对
- uncountable_nouns：移除自动误入的单复数对（reasons->reason 等）
- 删除 preposition_confusables（裸介词对误报风险高，介词规则改为 Rust 内建星期规则）
- rules 数组补 EN_SPELLING 条目 + spelling 分类
"""
import pathlib
import json

LEX = str(pathlib.Path(__file__).resolve().parents[1] / "lexicons" / "en_gec_lexicon.json")

MISSPELLINGS = [
    # JFLEG 高频
    ["exemple", "example"],
    ["thier", "their"],
    ["peole", "people"],
    ["poeple", "people"],
    ["successfull", "successful"],
    ["knowlege", "knowledge"],
    ["becouse", "because"],
    ["preffer", "prefer"],
    ["sosiety", "society"],
    ["creat", "create"],
    ["alot", "a lot"],
    ["cant", "can't"],
    ["dont", "don't"],
    ["studed", "studied"],
    ["studing", "studying"],
    ["comming", "coming"],
    ["planing", "planning"],
    ["somthing", "something"],
    ["totaly", "totally"],
    ["probbably", "probably"],
    # 常见非词（通用）
    ["wich", "which"],
    ["recieve", "receive"],
    ["seperate", "separate"],
    ["occured", "occurred"],
    ["definately", "definitely"],
    ["untill", "until"],
    ["enviroment", "environment"],
    ["goverment", "government"],
    ["neccessary", "necessary"],
    ["achive", "achieve"],
    ["arguement", "argument"],
    ["begining", "beginning"],
    ["calender", "calendar"],
    ["comittee", "committee"],
    ["concious", "conscious"],
    ["existance", "existence"],
    ["foriegn", "foreign"],
    ["grammer", "grammar"],
    ["immediatly", "immediately"],
    ["libary", "library"],
    ["maintainance", "maintenance"],
    ["milage", "mileage"],
    ["noticable", "noticeable"],
    ["occassion", "occasion"],
    ["pronounciation", "pronunciation"],
    ["publich", "publish"],
    ["realy", "really"],
    ["restraunt", "restaurant"],
    ["suprise", "surprise"],
    ["tommorow", "tomorrow"],
    ["tounge", "tongue"],
    ["truely", "truly"],
    ["wierd", "weird"],
]

UNCOUNTABLE = [
    ["informations", "information"],
    ["advices", "advice"],
    ["furnitures", "furniture"],
    ["luggages", "luggage"],
    ["equipments", "equipment"],
    ["knowledges", "knowledge"],
    ["a information", "information"],
    ["an advice", "advice"],
    ["a furniture", "furniture"],
    ["a luggage", "luggage"],
    ["fishes", "fish"],
]


def main():
    d = json.load(open(LEX))
    d["misspellings"] = MISSPELLINGS
    d["uncountable_nouns"] = UNCOUNTABLE
    d.pop("preposition_confusables", None)

    # rules 元数据：补 EN_SPELLING
    if not any(r["id"] == "EN_SPELLING" for r in d["rules"]):
        d["rules"].append({
            "id": "EN_SPELLING",
            "category": "spelling",
            "priority": 12,
            "enabled": True,
            "description": "拼写错误（高频错词表）",
            "examples": [
                {"wrong": "I dont like it.", "right": "I don't like it."},
                {"wrong": "This is a exemple.", "right": "This is an example."},
            ],
        })
    if not any(c["id"] == "spelling" for c in d["categories"]):
        d["categories"].append({"id": "spelling", "name": "拼写"})

    json.dump(d, open(LEX, "w"), ensure_ascii=False, indent=2)
    print(f"misspellings: {len(d['misspellings'])}, uncountable: {len(d['uncountable_nouns'])}, "
          f"rules: {len(d['rules'])}, categories: {[c['id'] for c in d['categories']]}")


if __name__ == "__main__":
    main()
