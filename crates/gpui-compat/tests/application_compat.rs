use gpui::{App, AppResourceProfile, Application, ApplicationHandle, Platform, QuitMode};
use std::{rc::Rc, sync::Arc};

#[allow(dead_code)]
fn compile_application_surface(
    application: Application,
    platform: Rc<dyn Platform>,
    http_client: Arc<dyn gpui::http_client::HttpClient>,
) {
    let _new: fn() -> Application = Application::new;
    let _headless: fn() -> Application = Application::headless;
    let _background_executor: fn() -> gpui::BackgroundExecutor = gpui::background_executor;
    let _current_platform: fn(bool) -> Rc<dyn Platform> = gpui::current_platform;

    let application = application
        .inaccessible()
        .with_assets(())
        .with_restart_arguments(Vec::new())
        .with_http_client(http_client)
        .with_resource_profile(AppResourceProfile::default())
        .with_quit_mode(QuitMode::Explicit);

    application.on_open_urls(|_| {});
    application.on_reopen(|cx: &mut App| {
        cx.set_window_appearance(Some(gpui::WindowAppearance::Dark));
        let _ = cx.window_appearance();
        cx.set_window_appearance(None);
    });
    let _ = application.background_executor();
    let _ = application.foreground_executor();
    let _ = application.text_system();
    let _ = application.path_for_auxiliary_executable("helper");

    let _: ApplicationHandle = application.run_embedded(|_: &mut App| {});
    Application::with_platform(platform).run(|_: &mut App| {});
}

#[test]
fn application_wrapper_exposes_the_compatibility_surface() {
    let _: fn(Application, Rc<dyn Platform>, Arc<dyn gpui::http_client::HttpClient>) =
        compile_application_surface;
}

#[test]
fn application_facade_mirrors_core_builder_signatures() {
    // Pin the current builder surface. New core builders must be added to this
    // manually maintained list; this test cannot discover new methods.
    let core_inaccessible: fn(gpui_core::Application) -> gpui_core::Application =
        gpui_core::Application::inaccessible;
    let facade_inaccessible: fn(Application) -> Application = Application::inaccessible;
    let _ = (core_inaccessible, facade_inaccessible);

    fn core_assets<A: gpui::AssetSource>(
        application: gpui_core::Application,
        assets: A,
    ) -> gpui_core::Application {
        application.with_assets(assets)
    }
    fn facade_assets<A: gpui::AssetSource>(application: Application, assets: A) -> Application {
        application.with_assets(assets)
    }
    let core_assets: fn(gpui_core::Application, ()) -> gpui_core::Application = core_assets;
    let facade_assets: fn(Application, ()) -> Application = facade_assets;
    let _ = (core_assets, facade_assets);

    let core_restart: fn(
        gpui_core::Application,
        Vec<std::ffi::OsString>,
    ) -> gpui_core::Application = gpui_core::Application::with_restart_arguments;
    let facade_restart: fn(Application, Vec<std::ffi::OsString>) -> Application =
        Application::with_restart_arguments;
    let _ = (core_restart, facade_restart);

    let core_http_client: fn(
        gpui_core::Application,
        Arc<dyn gpui_core::http_client::HttpClient>,
    ) -> gpui_core::Application = gpui_core::Application::with_http_client;
    let facade_http_client: fn(Application, Arc<dyn gpui::http_client::HttpClient>) -> Application =
        Application::with_http_client;
    let _ = (core_http_client, facade_http_client);

    fn core_resource_profile<P: Into<AppResourceProfile>>(
        application: gpui_core::Application,
        profile: P,
    ) -> gpui_core::Application {
        application.with_resource_profile(profile)
    }
    fn facade_resource_profile<P: Into<AppResourceProfile>>(
        application: Application,
        profile: P,
    ) -> Application {
        application.with_resource_profile(profile)
    }
    let core_resource_profile: fn(
        gpui_core::Application,
        gpui_core::AppResourceProfile,
    ) -> gpui_core::Application = core_resource_profile;
    let facade_resource_profile: fn(Application, AppResourceProfile) -> Application =
        facade_resource_profile;
    let _ = (core_resource_profile, facade_resource_profile);

    let core_quit: fn(gpui_core::Application, gpui_core::QuitMode) -> gpui_core::Application =
        gpui_core::Application::with_quit_mode;
    let facade_quit: fn(Application, QuitMode) -> Application = Application::with_quit_mode;
    let _ = (core_quit, facade_quit);
}

#[cfg(feature = "test-support")]
#[allow(dead_code)]
fn compile_real_visual_test_surface(context: gpui::RealVisualTestContext) {
    let _new: fn() -> gpui::RealVisualTestContext = gpui::RealVisualTestContext::new;
    let _new_if_supported: fn() -> Option<gpui::RealVisualTestContext> =
        gpui::RealVisualTestContext::new_if_supported;
    let _with_assets: fn(Arc<dyn gpui::AssetSource>) -> gpui::RealVisualTestContext =
        gpui::RealVisualTestContext::with_asset_source;

    let _: &Rc<gpui::AppCell> = &context.app;
    context.run_until_parked();
    context.advance_clock(std::time::Duration::ZERO);
    context.run(|_: &mut gpui::RealVisualTestContext| {});
}

#[cfg(feature = "test-support")]
#[test]
fn real_visual_wrapper_exposes_the_compatibility_surface() {
    let _: fn(gpui::RealVisualTestContext) = compile_real_visual_test_surface;
}
