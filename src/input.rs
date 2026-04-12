use openxr as xr;

/// Snapshot of one controller for a single frame.
pub struct ControllerState {
    /// World-space origin of the aim ray.
    pub ray_origin: glam::Vec3,
    /// World-space unit direction of the aim ray.
    pub ray_dir: glam::Vec3,
    /// True while the primary trigger is held.
    pub clicking: bool,
}

/// Manages OpenXR action sets for both hand controllers.
pub struct XrInput {
    pub action_set: xr::ActionSet,
    pose_action:    xr::Action<xr::Posef>,
    click_action:   xr::Action<bool>,
    aim_spaces:     [xr::Space; 2],
    hand_paths:     [xr::Path; 2],
}

impl XrInput {
    /// Create the action set and suggest bindings.
    /// Returns `None` if any mandatory step fails.
    pub fn new(
        instance: &xr::Instance,
        session:  &xr::Session<xr::Vulkan>,
    ) -> Option<Self> {
        let left  = instance.string_to_path("/user/hand/left").ok()?;
        let right = instance.string_to_path("/user/hand/right").ok()?;
        let hand_paths = [left, right];

        let action_set = instance
            .create_action_set("gameplay", "Gameplay", 0)
            .map_err(|e| eprintln!("XR input: create_action_set: {e}"))
            .ok()?;

        let pose_action = action_set
            .create_action::<xr::Posef>("aim_pose", "Aim Pose", &hand_paths)
            .map_err(|e| eprintln!("XR input: create aim_pose: {e}"))
            .ok()?;

        let click_action = action_set
            .create_action::<bool>("select_click", "Select Click", &hand_paths)
            .map_err(|e| eprintln!("XR input: create select_click: {e}"))
            .ok()?;

        // ── Oculus / Meta Touch controller bindings ────────────────────────
        // A (right) and X (left) are the primary face buttons used for selection.
        if let Ok(profile) = instance
            .string_to_path("/interaction_profiles/oculus/touch_controller")
        {
            let make_path = |s: &str| instance.string_to_path(s).ok();
            if let (Some(la), Some(ra), Some(lc), Some(rc)) = (
                make_path("/user/hand/left/input/aim/pose"),
                make_path("/user/hand/right/input/aim/pose"),
                make_path("/user/hand/left/input/x/click"),
                make_path("/user/hand/right/input/a/click"),
            ) {
                let _ = instance.suggest_interaction_profile_bindings(
                    profile,
                    &[
                        xr::Binding::new(&pose_action,  la),
                        xr::Binding::new(&pose_action,  ra),
                        xr::Binding::new(&click_action, lc),
                        xr::Binding::new(&click_action, rc),
                    ],
                );
            }
        }

        // ── KHR simple controller (generic fallback) ───────────────────────
        if let Ok(profile) =
            instance.string_to_path("/interaction_profiles/khr/simple_controller")
        {
            let make_path = |s: &str| instance.string_to_path(s).ok();
            if let (Some(la), Some(ra), Some(lc), Some(rc)) = (
                make_path("/user/hand/left/input/aim/pose"),
                make_path("/user/hand/right/input/aim/pose"),
                make_path("/user/hand/left/input/select/click"),
                make_path("/user/hand/right/input/select/click"),
            ) {
                let _ = instance.suggest_interaction_profile_bindings(
                    profile,
                    &[
                        xr::Binding::new(&pose_action,  la),
                        xr::Binding::new(&pose_action,  ra),
                        xr::Binding::new(&click_action, lc),
                        xr::Binding::new(&click_action, rc),
                    ],
                );
            }
        }

        session
            .attach_action_sets(&[&action_set])
            .map_err(|e| eprintln!("XR input: attach_action_sets: {e}"))
            .ok()?;

        let aim_spaces = [
            pose_action
                .create_space(session, left, xr::Posef::IDENTITY)
                .map_err(|e| eprintln!("XR input: create left aim space: {e}"))
                .ok()?,
            pose_action
                .create_space(session, right, xr::Posef::IDENTITY)
                .map_err(|e| eprintln!("XR input: create right aim space: {e}"))
                .ok()?,
        ];

        println!("XR: controller input ready");
        Some(Self { action_set, pose_action, click_action, aim_spaces, hand_paths })
    }

    /// Sync the action set and return the current state of both controllers.
    /// Index 0 = left hand, index 1 = right hand.
    pub fn poll(
        &self,
        session: &xr::Session<xr::Vulkan>,
        stage:   &xr::Space,
        time:    xr::Time,
    ) -> [Option<ControllerState>; 2] {
        let _ = session.sync_actions(&[xr::ActiveActionSet::new(&self.action_set)]);

        let mut out = [None, None];
        for (i, &hand_path) in self.hand_paths.iter().enumerate() {
            // Locate the aim space in stage coordinates.
            let loc = match self.aim_spaces[i].locate(stage, time) {
                Ok(l) => l,
                Err(_) => continue,
            };

            let flags = loc.location_flags;
            if !flags.contains(xr::SpaceLocationFlags::POSITION_VALID)
                || !flags.contains(xr::SpaceLocationFlags::ORIENTATION_VALID)
            {
                continue;
            }

            let p = loc.pose.position;
            let o = loc.pose.orientation;
            let pos = glam::Vec3::new(p.x, p.y, p.z);
            let rot = glam::Quat::from_xyzw(o.x, o.y, o.z, o.w);
            // Aim direction is -Z in the controller's local frame.
            let ray_dir = rot * glam::Vec3::NEG_Z;

            let clicking = self
                .click_action
                .state(session, hand_path)
                .map(|s| s.current_state && s.is_active)
                .unwrap_or(false);

            out[i] = Some(ControllerState { ray_origin: pos, ray_dir, clicking });
        }
        out
    }
}
