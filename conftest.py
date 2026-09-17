"""pytest 全局配置与共享 fixture（ENV-001）。

- 把项目根目录加入 `sys.path`，保证 `import strategy_engine` / `import test_mock` 可用。
- `noaa_fc` / `caiyun_fc`：`test_mock.py::test_formatter` 依赖的解析结果 fixture。
  这些 mock 数据内联在 `test_mock.py` 的解析用例中，这里直接复用，避免重复维护。
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


@pytest.fixture
def noaa_fc():
    """NOAA 解析 + 清洗后的预报（复用 test_mock 的内联 mock 数据）。"""
    import test_mock

    return test_mock.test_noaa_parse()


@pytest.fixture
def caiyun_fc():
    """彩云天气解析后的预报（复用 test_mock 的内联 mock 数据）。"""
    import test_mock

    return test_mock.test_caiyun_parse()
