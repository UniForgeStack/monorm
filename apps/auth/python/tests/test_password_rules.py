"""Password rules: the default list, the settings, custom rules. No database. The TypeScript twin is `password-rules.test.ts`."""

from pathlib import Path
from types import SimpleNamespace

import pytest

from monorm_auth import PasswordError, load_rules, password_help_texts, rules, validate_password

CUSTOM = '''
from monorm_auth import PasswordError


class NoCompany:
    def __init__(self, word):
        self.word = word

    def validate(self, password, user=None):
        if self.word in password.lower():
            return [PasswordError("password_has_company", f"The password has {self.word}.", {"word": self.word})]
        return []

    def help_text(self):
        return f"Your password can't have {self.word}."


class Later(NoCompany):
    async def validate(self, password, user=None):
        return NoCompany.validate(self, password, user)
'''


def project(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, settings: str = "") -> None:
    """A project directory with `monorm.toml` and the custom rules in `rules_mod`."""
    (tmp_path / "monorm.toml").write_text(settings)
    (tmp_path / "rules_mod.py").write_text(CUSTOM)
    monkeypatch.chdir(tmp_path)
    monkeypatch.syspath_prepend(str(tmp_path))


async def codes(password: str, user: object = None, **kw: object) -> list[str]:
    return [e.code for e in await validate_password(password, user, **kw)]  # type: ignore[arg-type]


ANNA = {"username": "annabelle", "email": "ann@example.com"}


async def test_numeric_means_decimal_digits_of_any_script_and_not_other_numerals(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    project(tmp_path, monkeypatch)
    assert await codes("٣٤٥٦٧٨٩٠١", ANNA) == ["password_entirely_numeric"]
    assert await codes("½½½½½½½½", ANNA) == []


async def test_the_default_rules_refuse_a_short_a_common_a_numeric_and_a_similar_password(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    project(tmp_path, monkeypatch)
    assert await codes("x7!Qr", ANNA) == ["password_too_short"]
    assert await codes("password", ANNA) == ["password_too_common"]
    assert await codes("PassWord ", ANNA) == ["password_too_common"]
    assert await codes("20250101", ANNA) == ["password_entirely_numeric"]
    assert await codes("annabelle1", ANNA) == ["password_too_similar"]
    assert await codes("correct horse battery", ANNA) == []
    assert await codes("1234", ANNA) == ["password_too_short", "password_too_common", "password_entirely_numeric"]


async def test_the_length_counts_code_points_after_nfc() -> None:
    four = [rules.MinLength(min=4)]
    assert await codes("ééé", rules=four) == ["password_too_short"]
    assert await codes("éééé", rules=four) == []
    assert await codes("😀😀😀", rules=four) == ["password_too_short"]
    assert await codes("😀😀😀😀", rules=four) == []


async def test_a_non_ascii_user_name_is_compared_after_nfc_and_lower_case() -> None:
    similar = [rules.UserSimilarity()]
    assert await codes("ÉMILIE2025", {"username": "émilie"}, rules=similar) == ["password_too_similar"]
    assert await codes("Zürich-Ωmega-77", {"username": "émilie"}, rules=similar) == []


async def test_the_error_has_a_code_a_message_and_params() -> None:
    errors = await validate_password("short", None, rules=[rules.MinLength(min=12)])
    assert errors == [
        PasswordError("password_too_short", "This password is too short. It must contain at least 12 characters.", {"min": 12})
    ]
    (similar,) = await validate_password("annabelle1", ANNA, rules=[rules.UserSimilarity()])
    assert similar.params == {"attribute": "username"}


async def test_the_user_is_a_model_a_dict_or_none() -> None:
    similar = [rules.UserSimilarity(attributes=["username", "first_name"])]
    assert await codes("mohammed1", SimpleNamespace(username="x", first_name="Mohammed"), rules=similar) == ["password_too_similar"]
    assert await codes("mohammed1", {"first_name": "Mohammed"}, rules=similar) == ["password_too_similar"]
    assert await codes("mohammed1", None, rules=similar) == []
    assert await codes("mohammed1", SimpleNamespace(username=None), rules=similar) == []


async def test_a_list_given_in_code_replaces_the_settings(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    project(tmp_path, monkeypatch, "[auth]\npassword_rules = [{rule = 'min_length', min = 30}]\n")
    assert await codes("password-long-enough") == ["password_too_short"]
    assert await codes("password-long-enough", rules=[rules.Common()]) == []


async def test_an_empty_list_turns_the_rules_off(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    project(tmp_path, monkeypatch, "[auth]\npassword_rules = []\n")
    assert await codes("1") == []
    assert password_help_texts() == []


async def test_a_custom_rule_from_the_settings_runs_in_list_order(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    project(
        tmp_path,
        monkeypatch,
        "[auth]\npassword_rules = [{rule = 'rules_mod:NoCompany', word = 'acme'}, {rule = 'min_length', min = 12}, "
        "{rule = 'rules_mod:Later', word = 'zed'}]\n",
    )
    assert await codes("Acme-zed") == ["password_has_company", "password_too_short", "password_has_company"]
    assert await codes("a quiet sunny afternoon") == []
    assert password_help_texts() == [
        "Your password can't have acme.",
        "Your password must contain at least 12 characters.",
        "Your password can't have zed.",
    ]


async def test_a_wrong_setting_is_an_error_when_the_rules_load(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    project(tmp_path, monkeypatch, "[auth]\npassword_rules = [{rule = 'min_length', mn = 3}]\n")
    with pytest.raises(ValueError, match=r"monorm.toml: `auth.password_rules\[0\].mn` is not an option of `min_length`"):
        load_rules()
    (tmp_path / "monorm.toml").write_text("[auth]\npassword_rules = [{rule = 'lenght'}]\n")
    with pytest.raises(ValueError, match=r"`auth.password_rules\[0\].rule` `lenght` is not a rule"):
        await validate_password("x")


async def test_a_custom_rule_that_cannot_be_imported_is_an_error_when_the_rules_load(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    project(tmp_path, monkeypatch, "[auth]\npassword_rules = [{rule = 'rules_mod:Missing'}]\n")
    with pytest.raises(RuntimeError, match="password rule `rules_mod:Missing`"):
        load_rules()


async def test_the_help_texts_follow_the_list(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    project(tmp_path, monkeypatch)
    assert password_help_texts()[0] == "Your password must contain at least 8 characters."
    assert len(password_help_texts()) == 4
    assert password_help_texts([rules.Numeric()]) == ["Your password can't be entirely numeric."]


async def test_a_common_file_replaces_the_bundled_list(tmp_path: Path) -> None:
    listing = tmp_path / "list.txt"
    listing.write_text("Hunter2hunter2\n")
    only = [rules.Common(path=str(listing))]
    assert await codes("HUNTER2hunter2", rules=only) == ["password_too_common"]
    assert await codes("password", rules=only) == []


async def test_a_wrong_option_in_code_is_an_error_when_the_rule_is_made() -> None:
    with pytest.raises(ValueError, match="min"):
        rules.MinLength(min=0)
    with pytest.raises(ValueError, match="max_similarity"):
        rules.UserSimilarity(max_similarity=2)
