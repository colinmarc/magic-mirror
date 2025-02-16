// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use bevy::{
    prelude::*,
    window::{PresentMode, PrimaryWindow, WindowResolution},
};
use clap::Parser;

const BLOCK_SIZE: f32 = 32.0;
const STARTING_POS: Vec3 = Vec3::new(-BLOCK_SIZE / 2.0, BLOCK_SIZE / 2.0, 0.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Resource)]
enum InputMode {
    Keyboard,
    Mouse,
    Gamepad,
}

#[derive(Debug, Parser)]
#[command(name = "latency-test")]
#[command(about = "The Magic Mirror latency test app", long_about = None)]
struct Cli {
    /// Mouse mode.
    #[arg(long)]
    mouse: bool,
    #[arg(long)]
    gamepad: bool,
}

#[derive(Component)]
struct Box(i8);

fn main() {
    let args = Cli::parse();

    let input_mode = match (args.mouse, args.gamepad) {
        (true, true) => {
            eprintln!("at most one of --mouse and --gamepad must be specified");
            std::process::exit(1);
        }
        (true, false) => InputMode::Mouse,
        (false, true) => InputMode::Gamepad,
        _ => InputMode::Keyboard,
    };

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Latency Test".to_string(),
                resolution: WindowResolution::new(BLOCK_SIZE * 8.0, BLOCK_SIZE * 8.0),
                present_mode: PresentMode::Mailbox,
                ..Default::default()
            }),
            ..Default::default()
        }))
        .insert_resource(ClearColor(Color::BLACK))
        .insert_resource(input_mode)
        .add_systems(Startup, setup)
        .add_systems(Update, move_box)
        .run();
}

fn setup(mut commands: Commands, input_mode: Res<InputMode>) {
    let starting_pos = if *input_mode == InputMode::Mouse || *input_mode == InputMode::Gamepad {
        STARTING_POS
    } else {
        // Offscreen.
        Vec3::new(BLOCK_SIZE * -100.0, BLOCK_SIZE * 100.0, 0.0)
    };

    commands.spawn(Camera2d::default());
    commands.spawn((
        Sprite {
            color: Color::WHITE,
            custom_size: Some(Vec2::new(BLOCK_SIZE, BLOCK_SIZE)),
            anchor: bevy::sprite::Anchor::TopLeft,
            ..default()
        },
        Transform::from_translation(starting_pos),
        Box(-1),
    ));
}

fn move_box(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    input_mode: Res<InputMode>,
    gamepads: Query<&Gamepad>,
    q_windows: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    mut q_box: Query<(&mut Box, &mut Transform)>,
) {
    let (mut b, mut transform) = q_box.single_mut();
    let window = q_windows.single();
    let (camera, camera_transform) = q_camera.single();

    match *input_mode {
        InputMode::Gamepad => {
            for gamepad in &gamepads {
                if gamepad.just_pressed(GamepadButton::South) {
                    transform.translation = STARTING_POS;
                }

                let rx = gamepad.get(GamepadAxis::RightStickX).unwrap();
                let ry = gamepad.get(GamepadAxis::RightStickY).unwrap();
                transform.translation += Vec3::new(rx, ry, 0.0);
            }
        }
        InputMode::Mouse => {
            if let Some(position) = window
                .cursor_position()
                .and_then(|cursor| camera.viewport_to_world(camera_transform, cursor).ok())
                .map(|ray| ray.origin.truncate())
            {
                transform.translation.x = position.x - BLOCK_SIZE / 2.0;
                transform.translation.y = position.y + BLOCK_SIZE / 2.0;
            }
        }
        InputMode::Keyboard => {
            if keyboard_input.just_pressed(KeyCode::Space) {
                b.0 = (b.0 + 1) % 64;
                let y = b.0 / 8;
                let x = b.0 % 8;

                transform.translation.x = BLOCK_SIZE * (-4.0 + x as f32);
                transform.translation.y = BLOCK_SIZE * (4.0 - y as f32);
            }
        }
    }
}
