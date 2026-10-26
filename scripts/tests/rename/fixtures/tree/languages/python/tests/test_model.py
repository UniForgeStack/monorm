import orm
from orm import Model
import monorm_core


def test_model():
    assert orm.Model is Model
