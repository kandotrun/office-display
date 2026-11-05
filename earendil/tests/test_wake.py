import pytest

import wake

# The design's table: what was said, what vosk-model-small-ja-0.22 wrote, and
# the question earendil should take from it ("" = wake, then listen).
MEASURED = [
    ("エンデルヒールならどう思う？", "エンゼル ヒール なら どう 思う", "どう思う"),
    (
        "エンデルヒールなら、締め切り前に徹夜するのってどう思う？",
        "エンゼル ヒール なら 締切 前 に 徹夜 する のっ て どう 思う",
        "締切前に徹夜するのってどう思う",
    ),
    ("エンデルヒールだったらどうする？", "エンゼル ビール だっ たら どう する", "どうする"),
    ("エンデルヒール的にはこの企画ありかな", "エンゼル ヒール 的 に は この 企画 あり 金", "この企画あり金"),
    ("エンデルヒールさん、相談があるんだけど", "エネルギー いる さん 相談 が ある ん だ けど", "相談があるんだけど"),
    ("エアレンディルならどうする？", "えー アレンジ いる なら どう する", "どうする"),
    (
        "エアレンディルなら、新しい技術を導入するべきかな",
        "えー で いる なら 新しい 技術 を 導入 する べき か な",
        "新しい技術を導入するべきかな",
    ),
    ("エアレンディルならどう言うかな", "えー で いる なら どう いう か だ", "どういうかだ"),
    ("エアレンディルならどう言うかな", "えー られん で いる なら どう いう か な", "どういうかな"),
    ("エアレンディルならどう言うかな", "やれ で いる なら どう いう か から", "どういうかから"),
]

MUST_NOT_WAKE = [
    "様 の お 昼ごはん どう する",  # 今日のお昼ご飯どうする？
    "それ なら どう 思う",  # それならどう思う？
    # 「ねえエンデルヒール」: only weak aliases, and no cue after them (rule 3);
    # ベルギービール is an ordinary word
    "解 連 ベルギー ビール",
    "で れる ビール",
    "エネルギー が 足りない",
    "ベルギー ビール が 好き",
    # ordinary talk one edit away from a short weak alias, with a cue after it
    "リーダー で いる の は 大変 だ よ ね",
    "メンバー で いる なら 協力 し て",
    "ユーザー で いる って 楽 だ よ",
    "フリー で いる なら いい ん じゃ ない",
    "エネルギー が いる の は 確か",
    "それ で いる は ず",
    "",
    "   ",
]


@pytest.mark.parametrize("said,caption,question", MEASURED, ids=[m[1] for m in MEASURED])
def test_measured_captions_wake_with_a_sensible_question(said, caption, question):
    hit = wake.detect(caption)
    assert hit is not None, said
    assert hit.question == question


@pytest.mark.parametrize("caption", MUST_NOT_WAKE)
def test_must_not_wake(caption):
    assert wake.detect(caption) is None


def test_normalise_folds_width_and_katakana_and_drops_spacing_and_punctuation():
    assert wake.normalise("エンゼル　ヒール、なら？") == "えんぜるひーるなら"
    assert wake.normalise("ｴﾝﾃﾞﾙﾋｰﾙ!") == "えんでるひーる"
    assert wake.normalise("ヴォイス・テスト") == "ゔぉいすてすと"
    assert wake.normalise("ＡＢＣ abc") == "abcabc"


def test_the_real_name_wakes_on_its_own():
    hit = wake.detect("エンデルヒール")
    assert hit is not None and hit.question == "" and hit.strong


def test_one_misheard_syllable_still_wakes_a_strong_alias():
    hit = wake.detect("エンデル ピール なら 休憩 し て いい")
    assert hit is not None
    assert hit.question == "休憩していい"
    assert hit.score == pytest.approx(1 - 1 / 7)


def test_two_misheard_syllables_do_not():
    assert wake.detect("メンデル ピール なら 休憩 し て いい") is None


def test_weak_alias_wakes_with_a_cue_within_four_characters():
    hit = wake.detect("で れる ビール さん 相談 が ある")
    assert hit is not None and not hit.strong
    assert hit.question == "相談がある"
    assert wake.detect("ベルギー ビール って どう 思う").question == "ってどう思う"


def test_weak_alias_with_the_cue_too_far_away_does_not_wake():
    assert wake.detect("ベルギー ビール が 美味しい なら") is None


def test_weak_aliases_must_match_exactly():
    # every measured weak caption matches exactly; one edit is ordinary speech
    assert wake.detect("やれ で いる なら どう いう か から").score == 1.0
    assert wake.detect("やれ で いる なら どう いう か から").alias == "やれでいる"
    assert wake.detect("あれ で いる なら どう いう か から") is None
    assert wake.detect("エネルギー が いる さん 相談 が ある") is None


def test_a_wake_without_a_question_listens():
    for caption in ("エンゼル ヒール", "ねえ エンゼル ヒール なら", "エネルギー いる さん"):
        hit = wake.detect(caption)
        assert hit is not None and hit.question == "", caption


def test_fewer_than_three_characters_left_is_no_question():
    assert wake.detect("エンゼル ヒール なら ねえ").question == ""
    assert wake.detect("エンゼル ヒール なら ねえ ねえ").question == "ねえねえ"


def test_the_question_keeps_the_original_script():
    hit = wake.detect("エンゼル ヒール なら コーヒー と 紅茶 どっち")
    assert hit.question == "コーヒーと紅茶どっち"


def test_question_is_capped():
    hit = wake.detect("エンゼル ヒール なら " + "あ" * 300)
    assert len(hit.question) == wake.MAX_QUESTION


def test_tidy_joins_japanese_words_but_keeps_latin_spacing():
    assert wake.tidy(" 締切 前 に  徹夜 ") == "締切前に徹夜"
    assert wake.tidy("Claude Code って どう") == "Claude Codeってどう"
    assert wake.tidy("ＡＩ　と\n仕事") == "AIと仕事"


def test_similarity_is_one_minus_edit_distance_over_alias_length():
    assert wake.similarity("えんでるひーる", "ねええんでるひーるなら") == (1.0, 9)
    score, end = wake.similarity("えんでるひーる", "えんでるぴーるなら")
    assert score == pytest.approx(1 - 1 / 7) and end == 7
    assert wake.similarity("えんでるひーる", "")[0] == 0.0
