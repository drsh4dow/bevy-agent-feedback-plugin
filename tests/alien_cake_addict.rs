mod support;

use bevy::{app::AppExit, prelude::*};
use bevy_agent_feedback_plugin::AgentFeedbackPlugin;
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

#[test]
#[ignore = "requires a graphics-capable environment"]
fn alien_cake_addict_accepts_agent_input_and_capture() {
    if support::skip_without_window_server() {
        return;
    }

    let root = support::artifact_root("alien-cake-addict");
    eprintln!("agent feedback artifacts: {}", root.display());
    let config = support::agent_config(&root);
    let capture_done = Arc::new(AtomicBool::new(false));
    let (result_sender, result_receiver) = mpsc::channel();

    let mut app = App::new();
    support::add_render_plugins(&mut app, "Alien Cake Addict agent feedback test");
    app.add_plugins(AgentFeedbackPlugin::new(config.clone()));
    alien_cake_addict::add_to_app(&mut app, capture_done.clone(), result_sender);

    let socket_addr = support::socket_addr(&config);
    let client = thread::spawn(move || drive_alien_cake_addict(socket_addr, capture_done));
    let exit = app.run();

    let app_result = result_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_else(|error| Err(format!("app exited without a test result: {error}")));
    let client_result = client
        .join()
        .unwrap_or_else(|_| Err("agent client panicked".to_string()));
    if let Err(error) = client_result {
        panic!("agent client failed: {error}");
    }
    if let Err(error) = app_result {
        panic!("Alien Cake Addict test failed: {error}");
    }
    assert_eq!(exit, AppExit::Success);
}

fn drive_alien_cake_addict(
    socket_addr: SocketAddr,
    capture_done: Arc<AtomicBool>,
) -> Result<(), String> {
    let (mut stream, mut reader) = support::connect_agent(socket_addr)?;
    support::send_request(
        &mut stream,
        &mut reader,
        r#"{"id":1,"command":"wait","frames":10}"#,
    )?;
    let before =
        support::send_request(&mut stream, &mut reader, r#"{"id":2,"command":"capture"}"#)?;
    let (before_path, before_pixels) = support::expect_png(&before)?;
    let key_down = support::send_request(
        &mut stream,
        &mut reader,
        r#"{"id":3,"command":"key_down","key":"ArrowUp"}"#,
    )?;
    support::expect_latest_capture(&key_down, &before_path)?;
    let wait = support::send_request(
        &mut stream,
        &mut reader,
        r#"{"id":4,"command":"wait","frames":45}"#,
    )?;
    support::expect_latest_capture(&wait, &before_path)?;
    let after = support::send_request(&mut stream, &mut reader, r#"{"id":5,"command":"capture"}"#)?;
    let (after_path, after_pixels) = support::expect_png(&after)?;
    if before_pixels == after_pixels {
        return Err(format!(
            "agent captures did not change after input: {} and {}",
            before_path.display(),
            after_path.display()
        ));
    }
    let key_up = support::send_request(
        &mut stream,
        &mut reader,
        r#"{"id":6,"command":"key_up","key":"ArrowUp"}"#,
    )?;
    support::expect_latest_capture(&key_up, &after_path)?;
    capture_done.store(true, Ordering::Relaxed);
    Ok(())
}

mod alien_cake_addict {
    use super::support::{Probe, finish_probe};
    use bevy::prelude::*;
    use rand::{RngExt, SeedableRng, rngs::StdRng};
    use std::{
        f32::consts::PI,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::Sender,
        },
    };

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, States)]
    enum GameState {
        #[default]
        Playing,
        GameOver,
    }

    #[derive(Resource)]
    struct BonusSpawnTimer(Timer);

    struct Cell {
        height: f32,
    }

    #[derive(Default)]
    struct Player {
        entity: Option<Entity>,
        i: usize,
        j: usize,
        move_cooldown: Timer,
    }

    #[derive(Default)]
    struct Bonus {
        entity: Option<Entity>,
        i: usize,
        j: usize,
        handle: Handle<WorldAsset>,
    }

    #[derive(Resource, Default)]
    struct Game {
        board: Vec<Vec<Cell>>,
        player: Player,
        bonus: Bonus,
        score: i32,
        cake_eaten: u32,
        camera_should_focus: Vec3,
        camera_is_focus: Vec3,
    }

    #[derive(Resource, Deref, DerefMut)]
    struct Random(StdRng);

    const BOARD_SIZE_I: usize = 14;
    const BOARD_SIZE_J: usize = 21;
    const RESET_FOCUS: [f32; 3] = [
        BOARD_SIZE_I as f32 / 2.0,
        0.0,
        BOARD_SIZE_J as f32 / 2.0 - 0.5,
    ];

    pub(super) fn add_to_app(
        app: &mut App,
        capture_done: Arc<AtomicBool>,
        result: Sender<Result<(), String>>,
    ) {
        app.init_resource::<Game>()
            .insert_resource(BonusSpawnTimer(Timer::from_seconds(
                5.0,
                TimerMode::Repeating,
            )))
            .insert_resource(Probe {
                capture_done,
                result: Some(result),
                max_frames: 1_800,
            })
            .init_state::<GameState>()
            .add_systems(Startup, setup_cameras)
            .add_systems(OnEnter(GameState::Playing), setup)
            .add_systems(
                Update,
                (
                    move_player,
                    focus_camera,
                    rotate_bonus,
                    scoreboard_system,
                    spawn_bonus,
                )
                    .run_if(in_state(GameState::Playing)),
            )
            .add_systems(OnEnter(GameState::GameOver), display_score)
            .add_systems(
                Update,
                game_over_keyboard.run_if(in_state(GameState::GameOver)),
            )
            .add_systems(Update, finish_when_agent_drove_player.after(move_player));
    }

    fn setup_cameras(mut commands: Commands, mut game: ResMut<Game>) {
        game.camera_should_focus = Vec3::from(RESET_FOCUS);
        game.camera_is_focus = game.camera_should_focus;
        commands.spawn((
            Camera3d::default(),
            Transform::from_xyz(
                -(BOARD_SIZE_I as f32 / 2.0),
                2.0 * BOARD_SIZE_J as f32 / 3.0,
                BOARD_SIZE_J as f32 / 2.0 - 0.5,
            )
            .looking_at(game.camera_is_focus, Vec3::Y),
        ));
    }

    fn setup(mut commands: Commands, asset_server: Res<AssetServer>, mut game: ResMut<Game>) {
        let mut rng = StdRng::seed_from_u64(19878367467713);

        game.cake_eaten = 0;
        game.score = 0;
        game.player.i = BOARD_SIZE_I / 2;
        game.player.j = BOARD_SIZE_J / 2;
        game.player.move_cooldown = Timer::from_seconds(0.3, TimerMode::Once);

        commands.spawn((
            DespawnOnExit(GameState::Playing),
            PointLight {
                intensity: 2_000_000.0,
                shadow_maps_enabled: true,
                range: 30.0,
                ..default()
            },
            Transform::from_xyz(4.0, 10.0, 4.0),
        ));

        let cell_scene =
            asset_server.load(GltfAssetLabel::Scene(0).from_asset("models/AlienCake/tile.glb"));
        game.board = (0..BOARD_SIZE_J)
            .map(|j| {
                (0..BOARD_SIZE_I)
                    .map(|i| {
                        let height = rng.random_range(-0.1..0.1);
                        commands.spawn((
                            DespawnOnExit(GameState::Playing),
                            Transform::from_xyz(i as f32, height - 0.2, j as f32),
                            WorldAssetRoot(cell_scene.clone()),
                        ));
                        Cell { height }
                    })
                    .collect()
            })
            .collect();

        game.player.entity =
            Some(
                commands
                    .spawn((
                        DespawnOnExit(GameState::Playing),
                        Transform {
                            translation: Vec3::new(
                                game.player.i as f32,
                                game.board[game.player.j][game.player.i].height,
                                game.player.j as f32,
                            ),
                            rotation: Quat::from_rotation_y(-PI / 2.),
                            ..default()
                        },
                        WorldAssetRoot(asset_server.load(
                            GltfAssetLabel::Scene(0).from_asset("models/AlienCake/alien.glb"),
                        )),
                    ))
                    .id(),
            );

        game.bonus.handle = asset_server
            .load(GltfAssetLabel::Scene(0).from_asset("models/AlienCake/cakeBirthday.glb"));

        commands.spawn((
            DespawnOnExit(GameState::Playing),
            Text::new("Score:"),
            TextFont {
                font_size: FontSize::Px(33.0),
                ..default()
            },
            TextColor(Color::srgb(0.5, 0.5, 1.0)),
            Node {
                position_type: PositionType::Absolute,
                top: px(5),
                left: px(5),
                ..default()
            },
        ));

        commands.insert_resource(Random(rng));
    }

    fn move_player(
        mut commands: Commands,
        keyboard_input: Res<ButtonInput<KeyCode>>,
        mut game: ResMut<Game>,
        mut transforms: Query<&mut Transform>,
        time: Res<Time>,
    ) {
        if game.player.move_cooldown.tick(time.delta()).is_finished() {
            let mut moved = false;
            let mut rotation = 0.0;

            if keyboard_input.pressed(KeyCode::ArrowUp) {
                if game.player.i < BOARD_SIZE_I - 1 {
                    game.player.i += 1;
                }
                rotation = -PI / 2.;
                moved = true;
            }
            if keyboard_input.pressed(KeyCode::ArrowDown) {
                if game.player.i > 0 {
                    game.player.i -= 1;
                }
                rotation = PI / 2.;
                moved = true;
            }
            if keyboard_input.pressed(KeyCode::ArrowRight) {
                if game.player.j < BOARD_SIZE_J - 1 {
                    game.player.j += 1;
                }
                rotation = PI;
                moved = true;
            }
            if keyboard_input.pressed(KeyCode::ArrowLeft) {
                if game.player.j > 0 {
                    game.player.j -= 1;
                }
                rotation = 0.0;
                moved = true;
            }

            if moved {
                game.player.move_cooldown.reset();
                *transforms.get_mut(game.player.entity.unwrap()).unwrap() = Transform {
                    translation: Vec3::new(
                        game.player.i as f32,
                        game.board[game.player.j][game.player.i].height,
                        game.player.j as f32,
                    ),
                    rotation: Quat::from_rotation_y(rotation),
                    ..default()
                };
            }
        }

        if let Some(entity) = game.bonus.entity
            && game.player.i == game.bonus.i
            && game.player.j == game.bonus.j
        {
            game.score += 2;
            game.cake_eaten += 1;
            commands.entity(entity).despawn();
            game.bonus.entity = None;
        }
    }

    fn focus_camera(
        time: Res<Time>,
        mut game: ResMut<Game>,
        transforms: Query<&Transform, Without<Camera3d>>,
        mut camera_transforms: Query<&mut Transform, With<Camera3d>>,
    ) {
        const SPEED: f32 = 2.0;
        if let (Some(player_entity), Some(bonus_entity)) = (game.player.entity, game.bonus.entity) {
            if let (Ok(player_transform), Ok(bonus_transform)) =
                (transforms.get(player_entity), transforms.get(bonus_entity))
            {
                game.camera_should_focus = player_transform
                    .translation
                    .lerp(bonus_transform.translation, 0.5);
            }
        } else if let Some(player_entity) = game.player.entity {
            if let Ok(player_transform) = transforms.get(player_entity) {
                game.camera_should_focus = player_transform.translation;
            }
        } else {
            game.camera_should_focus = Vec3::from(RESET_FOCUS);
        }

        let mut camera_motion = game.camera_should_focus - game.camera_is_focus;
        if camera_motion.length() > 0.2 {
            camera_motion *= SPEED * time.delta_secs();
            game.camera_is_focus += camera_motion;
        }
        for mut transform in camera_transforms.iter_mut() {
            *transform = transform.looking_at(game.camera_is_focus, Vec3::Y);
        }
    }

    fn spawn_bonus(
        time: Res<Time>,
        mut timer: ResMut<BonusSpawnTimer>,
        mut next_state: ResMut<NextState<GameState>>,
        mut commands: Commands,
        mut game: ResMut<Game>,
        mut rng: ResMut<Random>,
    ) {
        if !timer.0.tick(time.delta()).is_finished() {
            return;
        }

        if let Some(entity) = game.bonus.entity {
            game.score -= 3;
            commands.entity(entity).despawn();
            game.bonus.entity = None;
            if game.score <= -5 {
                next_state.set(GameState::GameOver);
                return;
            }
        }

        let mut bonus_cell = None;
        for _ in 0..BOARD_SIZE_I * BOARD_SIZE_J {
            let i = rng.random_range(0..BOARD_SIZE_I);
            let j = rng.random_range(0..BOARD_SIZE_J);
            if i != game.player.i || j != game.player.j {
                bonus_cell = Some((i, j));
                break;
            }
        }
        let Some((i, j)) = bonus_cell else {
            return;
        };

        game.bonus.i = i;
        game.bonus.j = j;
        game.bonus.entity = Some(
            commands
                .spawn((
                    DespawnOnExit(GameState::Playing),
                    Transform::from_xyz(
                        game.bonus.i as f32,
                        game.board[game.bonus.j][game.bonus.i].height + 0.2,
                        game.bonus.j as f32,
                    ),
                    WorldAssetRoot(game.bonus.handle.clone()),
                    children![(
                        PointLight {
                            color: Color::srgb(1.0, 1.0, 0.0),
                            intensity: 500_000.0,
                            range: 10.0,
                            ..default()
                        },
                        Transform::from_xyz(0.0, 2.0, 0.0),
                    )],
                ))
                .id(),
        );
    }

    fn rotate_bonus(game: Res<Game>, time: Res<Time>, mut transforms: Query<&mut Transform>) {
        if let Some(entity) = game.bonus.entity
            && let Ok(mut cake_transform) = transforms.get_mut(entity)
        {
            cake_transform.rotate_y(time.delta_secs());
            cake_transform.scale =
                Vec3::splat(1.0 + (game.score as f32 / 10.0 * ops::sin(time.elapsed_secs())).abs());
        }
    }

    fn scoreboard_system(game: Res<Game>, mut display: Single<&mut Text>) {
        display.0 = format!("Sugar Rush: {}", game.score);
    }

    fn game_over_keyboard(
        mut next_state: ResMut<NextState<GameState>>,
        keyboard_input: Res<ButtonInput<KeyCode>>,
    ) {
        if keyboard_input.just_pressed(KeyCode::Space) {
            next_state.set(GameState::Playing);
        }
    }

    fn display_score(mut commands: Commands, game: Res<Game>) {
        commands.spawn((
            DespawnOnExit(GameState::GameOver),
            Node {
                width: percent(100),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            children![(
                Text::new(format!("Cake eaten: {}", game.cake_eaten)),
                TextFont {
                    font_size: FontSize::Px(67.0),
                    ..default()
                },
                TextColor(Color::srgb(0.5, 0.5, 1.0)),
            )],
        ));
    }

    fn finish_when_agent_drove_player(
        mut probe: ResMut<Probe>,
        game: Res<Game>,
        mut app_exit: MessageWriter<AppExit>,
        mut frames: Local<u32>,
    ) {
        if probe.result.is_none() {
            return;
        }

        *frames += 1;
        if game.player.i > BOARD_SIZE_I / 2 && probe.capture_done.load(Ordering::Relaxed) {
            finish_probe(&mut probe, &mut app_exit, Ok(()));
        } else if *frames > probe.max_frames {
            let capture_done = probe.capture_done.load(Ordering::Relaxed);
            finish_probe(
                &mut probe,
                &mut app_exit,
                Err(format!(
                    "player cell was ({}, {}), capture_done={}",
                    game.player.i, game.player.j, capture_done
                )),
            );
        }
    }
}
