//! # 固定タイムステップでの物理シミュレーションと一人称カメラ移動のサンプル
//!
//! このサンプルは、Bevyにおいてプレイヤー入力を適切に処理し、
//! 固定タイムステップ（Fixed Timestep）で物理シミュレーションを更新し、
//! その結果をスムーズに画面表示（補間描画）する方法を示しています。
//!
//! ## 動機と背景（なぜ固定タイムステップと補間が必要か）
//!
//! ### 1. 単純な移動 (`transform.translation += velocity`) の問題点
//! 移動速度がフレームレート（FPS）に依存してしまいます。ハイスペックPC（高FPS）では高速に動き、
//! ロースペックPC（低FPS）では遅くなります。
//!
//! ### 2. Delta Time を掛け算する移動 (`transform.translation += velocity * time.delta_secs()`)
//! フレームレートに依存せず「1秒間に〇単位進む」速度表現になりますが、FPSが極端に低下した際に
//! 1フレームの移動量が大きくなりすぎ、壁を突き抜ける（トンネリング現象）などの物理破綻が発生します。
//!
//! ### 3. 固定タイムステップ（Fixed Timestep）の導入
//! 物理シミュレーションを常に一定の時間幅（例: 1/60秒ごと）で細かく刻んで計算します。
//! フレーム描画速度に関わらず安定した物理計算が行えます。
//!
//! ### 4. 描画のガタつき（カクつき）問題と位置補間（Interpolation）
//! 物理計算（例: 60Hz）と画面描画（例: 144Hzや不定FPS）のタイミングがずれると、
//! 物理計算が実行されないフレームと複数回実行されるフレームが生まれ、視覚的なカクつきが生じます。
//! 対策として「物理上の実際の位置（PhysicalTranslation）」と「見た目の表示位置（Transform）」を分離し、
//! 直前の物理位置と現在の物理位置の間を `lerp`（線形補間）して描画します。
//!
//! ## スケジュール構成と処理フロー
//!
//! 1. `RunFixedMainLoopSystems::BeforeFixedMainLoop`（毎フレーム・固定更新ループの前）:
//!    - `rotate_camera`: マウス移動量に応じてカメラを回転（プレイヤーの進行方向を確定させるため固定更新の前に実行）。
//!    - `accumulate_input`: WASDキー入力を読み取り、カメラの向きに合わせた移動速度ベクトルを計算。
//! 2. `FixedUpdate`（固定タイムステップ・1フレーム中に0回〜複数回実行）:
//!    - `advance_physics`: 速度と固定時間（dt）をもとに物理位置（`PhysicalTranslation`）を更新。
//! 3. `RunFixedMainLoopSystems::AfterFixedMainLoop`（毎フレーム・固定更新ループの後）:
//!    - `clear_input`: 固定更新が実行された後、蓄積入力をリセット。
//!    - `interpolate_rendered_transform`: 直前と現在の物理位置を余り時間（overstep_fraction）で補間して `Transform`（表示位置）に反映。
//!    - `translate_camera`: カメラの位置をプレイヤーの補間後の表示位置に同期。

use std::f32::consts::FRAC_PI_2;

use bevy::{color::palettes::tailwind, input::mouse::AccumulatedMouseMotion, prelude::*};
use bevy::{
    render::{RenderPlugin, settings::{WgpuSettings, RenderCreation, InstanceFlags}},
};

fn main() {
    App::new()
        // Bevyの標準プラグイン群（ウィンドウ生成、レンダラー、入力処理など）を追加
        .add_plugins((
            DefaultPlugins
            //.set(ImagePlugin::default_nearest())
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(
                    WgpuSettings {
                        instance_flags: InstanceFlags::ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER,
                        ..default()
                    },
                )),
                ..default()
            }),
        ))
        // フラグリソースの初期化（このフレームで固定タイムステップが実行されたかを追跡）
        .init_resource::<DidFixedTimestepRunThisFrame>()
        // アプリ起動時に1回だけ実行する初期化システム（UI、プレイヤー、3D環境の生成）
        .add_systems(Startup, (spawn_text, spawn_player, spawn_environment))
        // 毎フレームの開始時（PreUpdate）：固定タイムステップ実行フラグをクリア
        .add_systems(PreUpdate, clear_fixed_timestep_flag)
        // 固定タイムステップの開始時（FixedPreUpdate）：実行フラグを立てる
        .add_systems(FixedPreUpdate, set_fixed_time_step_flag)
        // 固定タイムステップ（FixedUpdate）：一定時間刻みで物理位置を計算
        .add_systems(FixedUpdate, advance_physics)
        // RunFixedMainLoopスケジュール：固定タイムステップループの前後に実行するシステム群を配置
        .add_systems(
            RunFixedMainLoop,
            (
                // --- 固定タイムステップ計算の前に実行する処理 ---
                (
                    // カメラの回転：物理計算で「カメラの向きに応じた移動」を行うため、固定更新の前に回転を確定させる
                    rotate_camera,
                    // キーボード入力の蓄積：WASD入力を取得し、現在のカメラ向きに合わせて物理速度を設定
                    accumulate_input,
                )
                    .chain() // 順序依存（rotate_camera -> accumulate_input）を保証
                    .in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop),

                // --- 固定タイムステップ計算の後に実行する処理 ---
                (
                    // 固定タイムステップが1回以上実行された場合のみ、入力バッファをクリア
                    clear_input.run_if(did_fixed_timestep_run_this_frame),
                    // 物理位置（前回と今回）を線形補間し、見た目の描画位置（Transform）をスムーズに更新
                    interpolate_rendered_transform,
                    // カメラ位置をプレイヤーの補間後の表示位置に追従させる
                    translate_camera,
                )
                    .chain() // 順序依存を保証
                    .in_set(RunFixedMainLoopSystems::AfterFixedMainLoop),
            ),
        )
        // アプリケーションループの開始
        .run();
}

/// プレイヤーのキーボード入力を蓄積するコンポーネント。
/// 前回物理シミュレーションが進んでから発生した全フレームの入力結果を保持します。
#[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
struct AccumulatedInput {
    /// WASD入力から得られる2D移動方向ベクトル (x: 左右, y: 前後)
    movement: Vec2,
}

/// 物理シミュレーションにおけるプレイヤーの移動速度（単位: m/sメートル毎秒）。
/// Deref, DerefMut パターンにより `velocity.0` だけでなく直接 Vec3 として扱いやすくしています。
#[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
struct Velocity(Vec3);

/// 物理シミュレーション内部でのプレイヤーの「現在の真の位置」。
/// 画面描画用の `Transform`（補間された位置）とは明確に分離して管理されます。
#[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
struct PhysicalTranslation(Vec3);

/// 直前の固定タイムステップにおける物理位置。
/// フレーム描画時に `PhysicalTranslation` との間で線形補間（lerp）を行うために使用します。
#[derive(Debug, Component, Clone, Copy, PartialEq, Default, Deref, DerefMut)]
struct PreviousPhysicalTranslation(Vec3);

/// カメラのマウス操作感度を保持するコンポーネント。
/// 横方向（yaw）と縦方向（pitch）の感度を別々に設定できます。
#[derive(Debug, Component, Deref, DerefMut)]
struct CameraSensitivity(Vec2);

impl Default for CameraSensitivity {
    fn default() -> Self {
        Self(
            // 実用上、縦方向（Y）よりも横方向（X）の感度をやや高く設定すると操作感が良くなります。
            // 実行時にユーザー設定やインスペクターから調整できるようにコンポーネント化されています。
            Vec2::new(0.003, 0.002),
        )
    }
}

/// プレイヤーおよび3Dカメラをスポーン（生成）するシステム。
/// ※カメラをプレイヤーの小Entity（Child）にせず独立して生成することで、
/// プレイヤーモデルの回転がカメラの回転に不要な悪影響を与えないように設計されています。
fn spawn_player(mut commands: Commands) {
    // 3DカメラEntityの生成（カメラコンポーネントと操作感度コンポーネント）
    commands.spawn((Camera3d::default(), CameraSensitivity::default()));

    // プレイヤーEntityの生成（見えない物理カプセル/オブジェクトとして定義）
    commands.spawn((
        Name::new("Player"),
        // プレイヤーのサイズ調整（スケール 0.3倍）
        Transform::from_scale(Vec3::splat(0.3)),
        // 物理計算および移動に必要な各種コンポーネントをアタッチ
        AccumulatedInput::default(),
        Velocity::default(),
        PhysicalTranslation::default(),
        PreviousPhysicalTranslation::default(),
    ));
}

/// 空間を飛んでいる感覚を体験できるように、グリッド状に配置された球体群と光源を生成するシステム。
fn spawn_environment(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // スカイブルー色のマテリアルアセットを作成
    let sphere_material = materials.add(Color::from(tailwind::SKY_200));
    // 半径0.3の球体メッシュアセットを作成
    let sphere_mesh = meshes.add(Sphere::new(0.3));

    // X, Y, Z 方向に球体を並べる配置ループ
    let spheres_in_x = 6;
    let spheres_in_y = 4;
    let spheres_in_z = 10;
    let distance = 3.0; // 球体同士の間隔

    for x in 0..spheres_in_x {
        for y in 0..spheres_in_y {
            for z in 0..spheres_in_z {
                // 原点を中心とするように計算されたワールド座標位置
                let translation = Vec3::new(
                    x as f32 * distance - (spheres_in_x as f32 - 1.0) * distance / 2.0,
                    y as f32 * distance - (spheres_in_y as f32 - 1.0) * distance / 2.0,
                    z as f32 * distance - (spheres_in_z as f32 - 1.0) * distance / 2.0,
                );
                // 球体Entityを生成
                commands.spawn((
                    Name::new("Sphere"),
                    Transform::from_translation(translation),
                    Mesh3d(sphere_mesh.clone()),
                    MeshMaterial3d(sphere_material.clone()),
                ));
            }
        }
    }

    // 太陽光（平行光源 DirectionalLight）の生成
    commands.spawn((
        DirectionalLight::default(),
        Transform::default().looking_to(Vec3::new(-1.0, -3.0, 0.5), Vec3::Y),
    ));
}

/// 操作方法を画面左下に表示する2D UIテキストを生成するシステム。
fn spawn_text(mut commands: Commands) {
    let font = TextFont {
        font_size: FontSize::Px(25.0),
        ..default()
    };
    commands.spawn((
        // Flexboxレイアウトノード（画面左下に絶対配置）
        Node {
            position_type: PositionType::Absolute,
            bottom: px(12),
            left: px(12),
            flex_direction: FlexDirection::Column,
            ..default()
        },
        // 子要素としてテキスト行を追加
        children![
            (Text::new("Move the player with WASD"), font.clone()),
            (Text::new("Rotate the camera with the mouse"), font)
        ],
    ));
}

/// マウスの移動量（AccumulatedMouseMotion）を取得し、カメラの回転（Transform.rotation）を更新するシステム。
fn rotate_camera(
    // 1フレーム中のマウス移動量の累積値リソース（Bevy 0.15+）
    accumulated_mouse_motion: Res<AccumulatedMouseMotion>,
    // Cameraコンポーネントを持つ唯一のEntityから Transform と CameraSensitivity を取得
    // Single<T> はワールド内に該当Entityがちょうど1個存在することを前提とする便利なクエリ型
    player: Single<(&mut Transform, &CameraSensitivity), With<Camera>>,
) {
    let (mut transform, camera_sensitivity) = player.into_inner();
    let delta = accumulated_mouse_motion.delta;

    if delta != Vec2::ZERO {
        // ※注意: マウス移動量には delta_time（時間経過）を掛け算しません。
        // マウス移動量は「前フレームからのピクセル移動距離そのもの」であるため、時間積算すると不自然に遅くなってしまいます。
        let delta_yaw = -delta.x * camera_sensitivity.x;   // 横回転（ヨー）
        let delta_pitch = -delta.y * camera_sensitivity.y; // 縦回転（ピッチ）

        // 現在のクォータニオン回転をオイラー角（Yaw, Pitch, Roll）に分解
        let (yaw, pitch, roll) = transform.rotation.to_euler(EulerRot::YXZ);
        let yaw = yaw + delta_yaw;

        // 【ジンバルロック・裏返り対策】
        // ピッチ（縦回転）が真上（+90度 = +π/2）または真下（-90度 = -π/2）に達すると、
        // 画面の上下反転やカメラの向きの不連続性（首が破綻する問題）が発生します。
        // これを防ぐため、真上・真下の少し手前（FRAC_PI_2 - 0.01）でピッチ角をクランプ（制限）します。
        const PITCH_LIMIT: f32 = FRAC_PI_2 - 0.01;
        let pitch = (pitch + delta_pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);

        // クランプされた新しいオイラー角からクォータニオンを再構築して代入
        transform.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, roll);
    }
    // println!("mouse = {:?}", accumulated_mouse_motion.delta);
}

/// キーボードのWASD入力を読み取り、カメラの向いている方向に基づいた移動速度（Velocity）に変換して蓄積するシステム。
fn accumulate_input(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    // プレイヤーの入力を保持する唯一のEntityを参照
    player: Single<(&mut AccumulatedInput, &mut Velocity)>,
    // カメラの向き（回転角）を参照
    camera: Single<&Transform, With<Camera>>,
) {
    // 歩行速度（Bevyの3D空間の基本単位は「メートル」なので 4.0 m/s = 秒速4m）
    const SPEED: f32 = 4.0;
    let (mut input, mut velocity) = player.into_inner();

    // 毎フレーム入力を一度リセット（押し続け入力に対応）
    input.movement = Vec2::ZERO;
    if keyboard_input.pressed(KeyCode::KeyW) {
        input.movement.y += 1.0;
    }
    if keyboard_input.pressed(KeyCode::KeyS) {
        input.movement.y -= 1.0;
    }
    if keyboard_input.pressed(KeyCode::KeyA) {
        input.movement.x -= 1.0;
    }
    if keyboard_input.pressed(KeyCode::KeyD) {
        input.movement.x += 1.0;
    }

    // 2D入力（WASD）をBevyの3D座標系（右手系：前方向は -Z 軸、上方向は +Y 軸）に変換
    // Wキー（+Y方向入力）を押した時、前に進むため Z 軸方向へは -input.movement.y と反転させます
    let input_3d = Vec3 {
        x: input.movement.x,
        y: 0.0,
        z: -input.movement.y,
    };

    // カメラの回転クォータニオンを入力ベクトルに乗算し、
    // 「カメラが見ている向きにおける前左右ベクトル」へ回転変換
    let rotated_input = camera.rotation * input_3d;

    // 斜め移動時に速度が √2 倍（約1.414倍）にならないよう正規化し、移動速度 SPEED を掛け算します。
    // ※ .normalize_or_zero() ではなく clamp_length_max(1.0) を使用する理由は、
    // ゲームパッドのアナログスティックで浅く倒した際（長さを1.0未満に保ちたい時）に対応するためです。
    velocity.0 = rotated_input.clamp_length_max(1.0) * SPEED;
}

/// このフレームで固定タイムステップが実行されたかどうかを示すフラグリソース。
#[derive(Resource, Debug, Deref, DerefMut, Default)]
pub struct DidFixedTimestepRunThisFrame(bool);

/// 毎フレームの開始時（PreUpdate）にフラグを false にリセットするシステム。
fn clear_fixed_timestep_flag(
    mut did_fixed_timestep_run_this_frame: ResMut<DidFixedTimestepRunThisFrame>,
) {
    did_fixed_timestep_run_this_frame.0 = false;
}

/// 固定タイムステップ実行時（FixedPreUpdate）にフラグを true に設定するシステム。
fn set_fixed_time_step_flag(
    mut did_fixed_timestep_run_this_frame: ResMut<DidFixedTimestepRunThisFrame>,
) {
    did_fixed_timestep_run_this_frame.0 = true;
}

/// 条件付きシステム実行（run_if）のための判定関数。
/// 固定タイムステップがこのフレームで実行された場合のみ true を返します。
fn did_fixed_timestep_run_this_frame(
    did_fixed_timestep_run_this_frame: Res<DidFixedTimestepRunThisFrame>,
) -> bool {
    did_fixed_timestep_run_this_frame.0
}

/// 固定タイムステップでの物理計算が完了した後、蓄積された入力をクリアするシステム。
fn clear_input(mut input: Single<&mut AccumulatedInput>) {
   **input = AccumulatedInput::default();
}

/// 固定タイムステップ（FixedUpdate）ごとに物理シミュレーションを1ステップ進めるシステム。
/// （※このシステムは1画面フレームの間に 0回、1回、または複数回 呼び出される可能性があります）
fn advance_physics(
    // FixedUpdate 内では Res<Time> は自動的に Res<Time<Fixed>> と同等になります（明示的に型指定しています）
    fixed_time: Res<Time<Fixed>>,
    mut query: Query<(
        &mut PhysicalTranslation,
        &mut PreviousPhysicalTranslation,
        &Velocity,
    )>,
) {
    for (mut current_physical_translation, mut previous_physical_translation, velocity) in
        query.iter_mut()
    {
        // 1. 今回の位置を「前回の物理位置」として退避
        previous_physical_translation.0 = current_physical_translation.0;
        // 2. 速度 × 固定タイムステップ時間（dt）を加えて「現在の物理位置」を更新
        current_physical_translation.0 += velocity.0 * fixed_time.delta_secs();
    }
}

/// 物理計算結果（前回と今回）の間を線形補間（lerp）し、
/// 見た目の描画用位置（Transform.translation）を滑らかに更新するシステム。
fn interpolate_rendered_transform(
    fixed_time: Res<Time<Fixed>>,
    mut query: Query<(
        &mut Transform,
        &PhysicalTranslation,
        &PreviousPhysicalTranslation,
    )>,
) {
    for (mut transform, current_physical_translation, previous_physical_translation) in
        query.iter_mut()
    {
        let previous = previous_physical_translation.0;
        let current = current_physical_translation.0;

        // fixed_time.overstep_fraction() は「次の固定ステップまでの余り時間の割合（0.0 〜 1.0）」を返します。
        // 例: 物理更新と次の物理更新のちょうど中間のタイミングで描画される場合、alpha は 0.5 になります。
        let alpha = fixed_time.overstep_fraction();

        // 前回位置と今回位置の間を alpha の割合で線形補間（lerp: Linear Interpolation）
        let rendered_translation = previous.lerp(current, alpha);

        // 計算したスムーズな位置をレンダリング用の Transform に反映
        transform.translation = rendered_translation;
    }
}

/// カメラの位置を、プレイヤーの補間後の表示位置（Transform.translation）に追従させるシステム。
fn translate_camera(
    // カメラの Transform
    mut camera: Single<&mut Transform, With<Camera>>,
    // プレイヤーの Transform（カメラ以外の AccumulatedInput を持つEntity）
    player: Single<&Transform, (With<AccumulatedInput>, Without<Camera>)>,
) {
    // カメラの位置をプレイヤーの位置と同一に更新（一人称視点）
    camera.translation = player.translation;
}
