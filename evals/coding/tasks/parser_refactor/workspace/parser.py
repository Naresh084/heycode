def parse_pairs(value: str) -> dict[str, str]:
    result: dict[str, str] = {}
    fields = value.split(",")
    for field in fields:
        if "=" not in field:
            raise ValueError("missing equals sign")
        parts = field.split("=", 1)
        key = parts[0].strip()
        item = parts[1].strip()
        if key == "":
            raise ValueError("invalid key")
        if key in result:
            raise ValueError("invalid key")
        result[key] = item
    return result

