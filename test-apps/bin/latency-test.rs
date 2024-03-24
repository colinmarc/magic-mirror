// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use bevy::{
    prelude::*,
    window::{PresentMode, PrimaryWindow, WindowResolution},
};
use clap::Parser;

const SIZE: f32 = 32.0;

#[derive(Debug, Parser, Resource)]
#[command(name = "latency-test")]
#[command(about = "The Magic Mirror latency test app", long_about = None)]
struct Cli {
    /// Mouse mode.
    #[arg(long)]
    mouse: bool,
}

#[derive(Component)]
struct Box(i8);

fn main() {
    let args = Cli::parse();

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Latency Test".to_string(),
                resolution: WindowResolution::new(SIZE * 8.0, SIZE * 8.0),
                present_mode: PresentMode::Immediate,
                ..Default::default()
            }),
            ..Default::default()
        }))
        .insert_resource(ClearColor(Color::BLACK))
        .insert_resource(args)
        .add_systems(Startup, setup)
        .add_systems(Update, (move_box, bevy::window::close_on_esc))
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn(Camera2dBundle::default());
    commands.spawn((
        SpriteBundle {
            sprite: Sprite {
                color: Color::WHITE,
                custom_size: Some(Vec2::new(SIZE, SIZE)),
                anchor: bevy::sprite::Anchor::TopLeft,
                ..default()
            },
            transform: Transform::from_translation(Vec3::new(SIZE * -100.0, SIZE * 100.0, 0.0)),
            ..default()
        },
        Box(-1),
    ));
}

fn move_box(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    args: Res<Cli>,
    q_windows: Query<&Window, With<PrimaryWindow>>,
    q_camera: Query<(&Camera, &GlobalTransform)>,
    mut q_box: Query<(&mut Box, &mut Transform)>,
) {
    let (mut b, mut transform) = q_box.single_mut();
    let window = q_windows.single();
    let (camera, camera_transform) = q_camera.single();

    if args.mouse {
        if let Some(position) = window
            .cursor_position()
            .and_then(|cursor| camera.viewport_to_world(camera_transform, cursor))
            .map(|ray| ray.origin.truncate())
        {
            transform.translation.x = position.x - SIZE / 2.0;
            transform.translation.y = position.y + SIZE / 2.0;
        }
    } else if keyboard_input.just_pressed(KeyCode::Space) {
        b.0 = (b.0 + 1) % 64;
        let y = b.0 / 8;
        let x = b.0 % 8;

        transform.translation.x = SIZE * (-4.0 + x as f32);
        transform.translation.y = SIZE * (4.0 - y as f32);
    }
}
