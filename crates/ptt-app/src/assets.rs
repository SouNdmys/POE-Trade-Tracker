//! Serves the SVG icons that gpui-component asks for by path.
//!
//! gpui 的 `svg()` 只认 `Application::with_assets` 注册的资源源：没有它，
//! 所有图标都静默画成空白——下拉按钮的箭头"消失"就是这么来的。发布的
//! gpui-component crate 不带图标文件，所以需要的几张在这里自己嵌。

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "icons/chevron-down.svg" => Some(Cow::Borrowed(include_bytes!(
                "../assets/icons/chevron-down.svg"
            ))),
            "icons/check.svg" => Some(Cow::Borrowed(include_bytes!("../assets/icons/check.svg"))),
            _ => None,
        })
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod assets_tests {
    use super::*;

    // 组件按这两个路径要图（gpui-component icon.rs 里写死的），路径服务
    // 不到就是看不见的空白,所以锁住它们。
    #[test]
    fn serves_the_icons_the_components_ask_for() {
        for path in ["icons/chevron-down.svg", "icons/check.svg"] {
            let bytes = Assets
                .load(path)
                .unwrap()
                .unwrap_or_else(|| panic!("{path} must be embedded"));
            assert!(!bytes.is_empty());
        }
    }
}
