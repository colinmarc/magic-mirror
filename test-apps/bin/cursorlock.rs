// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

// Adapted from:
// https://bevyengine.org/examples/camera/first-person-view-model/

use bevy::color::palettes::tailwind;
use bevy::input::mouse::MouseMotion;
use bevy::pbr::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use bevy::window::{CursorGrabMode, PrimaryWindow};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_systems(Startup, (spawn_view_model, spawn_world_model, spawn_lights))
        .add_systems(Update, (move_player, toggle_cursor_lock, update_cursor))
        .insert_resource(CursorLocked(true))
        .run();
}

#[derive(Debug, Component)]
struct Player;

#[derive(Debug, Component)]
struct WorldModelCamera;

#[derive(Debug, Resource)]
struct CursorLocked(bool);

/// Used implicitly by all entities without a `RenderLayers` component.
/// Our world model camera and all objects other than the player are on this
/// layer. The light source belongs to both layers.
const DEFAULT_RENDER_LAYER: usize = 0;

/// Used by the view model camera and the player's arm.
/// The light source belongs to both layers.
const VIEW_MODEL_RENDER_LAYER: usize = 1;

fn spawn_view_model(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let arm = meshes.add(Cuboid::new(0.1, 0.1, 0.5));
    let arm_material = materials.add(Color::from(tailwind::TEAL_200));

    commands
        .spawn((
            Player,
            SpatialBundle {
                transform: Transform::from_xyz(0.0, 1.0, 0.0),
                ..default()
            },
        ))
        .with_children(|parent| {
            parent.spawn((
                WorldModelCamera,
                Camera3dBundle {
                    projection: PerspectiveProjection {
                        fov: 90.0_f32.to_radians(),
                        ..default()
                    }
                    .into(),
                    ..default()
                },
            ));

            // Spawn view model camera.
            parent.spawn((
                Camera3dBundle {
                    camera: Camera {
                        // Bump the order to render on top of the world model.
                        order: 1,
                        ..default()
                    },
                    projection: PerspectiveProjection {
                        fov: 70.0_f32.to_radians(),
                        ..default()
                    }
                    .into(),
                    ..default()
                },
                // Only render objects belonging to the view model.
                RenderLayers::layer(VIEW_MODEL_RENDER_LAYER),
            ));

            // Spawn the player's right arm.
            parent.spawn((
                MaterialMeshBundle {
                    mesh: arm,
                    material: arm_material,
                    transform: Transform::from_xyz(0.2, -0.1, -0.25),
                    ..default()
                },
                // Ensure the arm is only rendered by the view model camera.
                RenderLayers::layer(VIEW_MODEL_RENDER_LAYER),
                // The arm is free-floating, so shadows would look weird.
                NotShadowCaster,
            ));
        });
}

fn spawn_world_model(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let floor = meshes.add(Plane3d::new(Vec3::Y, Vec2::splat(10.0)));
    let cube = meshes.add(Cuboid::new(2.0, 0.5, 1.0));
    let material = materials.add(Color::WHITE);

    // The world model camera will render the floor and the cubes spawned in this
    // system. Assigning no `RenderLayers` component defaults to layer 0.

    commands.spawn(MaterialMeshBundle {
        mesh: floor,
        material: material.clone(),
        ..default()
    });

    commands.spawn(MaterialMeshBundle {
        mesh: cube.clone(),
        material: material.clone(),
        transform: Transform::from_xyz(0.0, 0.25, -3.0),
        ..default()
    });

    commands.spawn(MaterialMeshBundle {
        mesh: cube,
        material,
        transform: Transform::from_xyz(0.75, 1.75, 0.0),
        ..default()
    });
}

fn spawn_lights(mut commands: Commands) {
    commands.spawn((
        PointLightBundle {
            point_light: PointLight {
                color: Color::from(tailwind::ROSE_300),
                shadows_enabled: true,
                ..default()
            },
            transform: Transform::from_xyz(-2.0, 4.0, -0.75),
            ..default()
        },
        // The light source illuminates both the world model and the view model.
        RenderLayers::from_layers(&[DEFAULT_RENDER_LAYER, VIEW_MODEL_RENDER_LAYER]),
    ));
}

fn move_player(
    mut mouse_motion: EventReader<MouseMotion>,
    mut player: Query<&mut Transform, With<Player>>,
    cursor_locked: Res<CursorLocked>,
) {
    if !cursor_locked.0 {
        return;
    }

    let mut transform = player.single_mut();
    for motion in mouse_motion.read() {
        let yaw = -motion.delta.x * 0.003;
        let pitch = -motion.delta.y * 0.002;
        // Order of rotations is important, see <https://gamedev.stackexchange.com/a/136175/103059>
        transform.rotate_y(yaw);
        transform.rotate_local_x(pitch);
    }
}

fn toggle_cursor_lock(input: Res<ButtonInput<KeyCode>>, mut cursor_locked: ResMut<CursorLocked>) {
    if input.just_pressed(KeyCode::Escape) {
        cursor_locked.0 = !cursor_locked.0
    }
}

fn update_cursor(
    cursor_locked: Res<CursorLocked>,
    mut q_windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if !cursor_locked.is_changed() {
        return;
    }

    let mut primary_window = q_windows.single_mut();
    if cursor_locked.0 {
        primary_window.cursor.grab_mode = CursorGrabMode::Locked;
        primary_window.cursor.visible = false;
    } else {
        primary_window.cursor.grab_mode = CursorGrabMode::None;
        primary_window.cursor.visible = true;
    }
}
