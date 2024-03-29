use std::rc::Rc;

use cstr::cstr;
use eyre::Result;
use glam::{Mat4, Quat, Vec3};

use shader_constants::CONSTS;

use crate::{
    app_settings::{AppSettings, MaterialSrc},
    brdf_raw::BrdfType,
    camera::Camera,
    ogl::{texture::GlTexture, uniform_buffer::UniformBuffer, vao::Vao},
    resources::Resources,
    scene::{Mesh, Node, Primitive, Scene},
};

mod cubemap;
pub mod ibl;
mod lighting;
pub mod material;
pub mod pbr_settings;
mod shaders;
mod transforms;

pub use material::PbrMaterial;

use self::{
    lighting::Lighting,
    pbr_settings::PbrSettings,
    shaders::{DataDrivenDefines, PbrDefines, Shaders},
    transforms::Transforms,
};

/// A component responsible for rendering the scene.
pub struct Renderer {
    shaders: Shaders,
    /// Current MVP transformation matrices
    transforms: UniformBuffer<Transforms>,
    /// Current mesh material
    material: UniformBuffer<PbrMaterial>,
    /// Current lighting settings
    lighting: UniformBuffer<Lighting>,
    /// Runtime rendering settings
    pbr_settings: UniformBuffer<PbrSettings>,
    cube: Vao,

    dfg_lut: GlTexture,
}

pub struct RenderCtx<'r> {
    pub app_settings: &'r mut AppSettings,
    pub res: &'r mut Resources,
    pub camera: &'r mut dyn Camera,
}

impl Renderer {
    /// Create a new renderer
    pub fn new() -> Result<Self> {
        let dfg_lut = ibl::dfg_integration()?;

        Ok(Self {
            shaders: Shaders::new()?,
            transforms: UniformBuffer::new(Transforms::new_indentity()),
            material: UniformBuffer::new(PbrMaterial::new()),
            lighting: UniformBuffer::new(Lighting::new()),
            pbr_settings: UniformBuffer::new(PbrSettings::new()),
            cube: cubemap::init_cube(),
            dfg_lut,
        })
    }

    /// Render a new frame
    pub fn render(&mut self, rctx: &mut RenderCtx) -> Result<()> {
        self.reset_gl_state(rctx);
        self.update_uniforms(rctx)?;

        self.render_lights()?;

        // Ugly borrowing hack...
        let mut scenes = std::mem::take(&mut rctx.res.scenes);
        let selected_scene = rctx.app_settings.selected_scene;
        let scene = scenes[selected_scene].load()?;

        let transform = Self::calc_model_transform(scene, rctx.app_settings);
        self.render_gltf_node(&scene.root, transform, rctx)?;

        rctx.res.scenes = scenes;

        self.draw_cubemap(rctx)?;

        Ok(())
    }

    fn update_uniforms(&mut self, rctx: &mut RenderCtx) -> Result<()> {
        self.update_brdf(rctx)?;

        let selected_scene = rctx.app_settings.selected_scene;

        self.pbr_settings.inner = rctx.app_settings.pbr_settings;
        self.pbr_settings.update();

        // TODO: let this be user-configurable
        let persp = Mat4::perspective_rh_gl(
            f32::to_radians(60.),
            rctx.app_settings.viewport_dim.width / rctx.app_settings.viewport_dim.height,
            0.1,
            1000.,
        );

        let scene = rctx.res.scenes[selected_scene].load()?;
        self.transforms.inner.projection = persp;
        self.transforms.inner.view = rctx.camera.view_mat();
        self.transforms.inner.model = Self::calc_model_transform(scene, rctx.app_settings);
        self.transforms.update();

        self.lighting.inner.cam_pos = rctx.camera.get_pos().extend(0.0);
        self.lighting.update();

        Ok(())
    }

    fn calc_model_transform(scene: &mut Scene, app_settings: &mut AppSettings) -> Mat4 {
        let scale = Vec3::splat(app_settings.model_scale);
        let translation = app_settings.model_translation;
        let rot = app_settings.model_rotation;
        let rotation = Quat::from_euler(
            glam::EulerRot::XYZ,
            rot.x.to_radians(),
            rot.y.to_radians(),
            rot.z.to_radians(),
        );

        let settings_transform =
            Mat4::from_scale_rotation_translation(scale, rotation, translation);
        settings_transform * scene.transform
    }

    fn update_brdf(&mut self, rctx: &mut RenderCtx) -> Result<()> {
        let material_src = rctx.app_settings.material_src;

        match material_src {
            MaterialSrc::MerlBrdf => {
                let selected_brdf = rctx.app_settings.selected_merl_brdf;
                let brdf = rctx.res.merl_brdfs[selected_brdf].load()?;
                brdf.ssbo.bind();
            }
            MaterialSrc::UtiaBrdf => {
                let selected_brdf = rctx.app_settings.selected_utia_brdf;
                let brdf = rctx.res.utia_brdfs[selected_brdf].load()?;
                brdf.ssbo.bind();
            }
            _ => (),
        };

        Ok(())
    }

    fn draw_cubemap(&mut self, rctx: &mut RenderCtx) -> Result<()> {
        let selected_envmap = rctx.app_settings.selected_envmap;
        let cubemap = rctx.res.envmaps[selected_envmap].load()?;

        let texid = if rctx.app_settings.blur_background {
            cubemap.irradiance_tex.id
        } else {
            cubemap.cubemap_tex.id
        };

        self.shaders.cubemap_shader.use_shader(|| unsafe {
            gl::BindTextureUnit(0, texid);

            gl::BindVertexArray(self.cube.id);
            gl::DrawElements(
                gl::TRIANGLES,
                cubemap::INDICES.len() as _,
                gl::UNSIGNED_BYTE,
                0 as _,
            );
            gl::BindVertexArray(0);
        });

        Ok(())
    }

    fn reset_gl_state(&self, rctx: &mut RenderCtx) {
        unsafe {
            gl::Enable(gl::DEPTH_TEST);
            gl::DepthFunc(gl::LEQUAL);

            gl::Enable(gl::CULL_FACE);
            gl::CullFace(gl::BACK);
            gl::FrontFace(gl::CCW);

            gl::Viewport(
                rctx.app_settings.viewport_dim.min_x as i32,
                rctx.app_settings.viewport_dim.min_y as i32,
                rctx.app_settings.viewport_dim.width as i32,
                rctx.app_settings.viewport_dim.height as i32,
            );
            gl::ClearColor(0.15, 0.15, 0.15, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT | gl::DEPTH_BUFFER_BIT);

            gl::Enable(gl::TEXTURE_CUBE_MAP_SEAMLESS);

            // TODO(high): enable / disable alpha blending based on GLTF
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
        }
    }

    /// Recursive - traverses the node hierarchy and handles each node.
    fn render_gltf_node(
        &mut self,
        node: &Node,
        outer_transform: Mat4,
        rctx: &mut RenderCtx,
    ) -> Result<()> {
        let next_level_transform = outer_transform * node.transform;

        if let Some(mesh) = &node.mesh {
            self.render_mesh(mesh, next_level_transform, rctx)?;
        }

        for node in &node.children {
            self.render_gltf_node(node, next_level_transform, rctx)?;
        }

        Ok(())
    }

    /// Renders the mesh of a node
    fn render_mesh(
        &mut self,
        mesh: &Mesh,
        node_transform: Mat4,
        rctx: &mut RenderCtx,
    ) -> Result<()> {
        self.transforms.inner.model = node_transform;
        self.transforms.update();

        for primitive in &mesh.primitives {
            self.bind_textures(primitive, rctx)?;
            self.set_material(primitive, rctx);

            let shader = match rctx.app_settings.material_src {
                b @ (MaterialSrc::MerlBrdf | MaterialSrc::UtiaBrdf) => {
                    let brdf_typ = match b {
                        MaterialSrc::MerlBrdf => BrdfType::Merl,
                        MaterialSrc::UtiaBrdf => BrdfType::Utia,
                        _ => unreachable!(),
                    };
                    let defines = DataDrivenDefines::from_prim_brdf(primitive, brdf_typ);
                    self.shaders.data_based_shaders.get_shader(defines)?
                }
                _ => {
                    let defines = PbrDefines::from_prim(primitive);
                    self.shaders.pbr_shaders.get_shader(defines)?
                }
            };

            shader.use_shader(|| {
                Self::draw_mesh(primitive);
            })
        }

        Ok(())
    }

    fn bind_textures(&mut self, primitive: &Primitive, rctx: &mut RenderCtx) -> Result<()> {
        let bind_texture_unit = |tex: &Option<Rc<GlTexture>>, port: u32| {
            if let Some(tex) = tex {
                unsafe {
                    gl::BindTextureUnit(port, tex.id);
                }
            }
        };

        // Do this first as this can use texture units 0-1
        let selected_envmap = rctx.app_settings.selected_envmap;
        let iblenv = rctx.res.envmaps[selected_envmap].load()?;

        let tp = CONSTS.texture_ports;
        bind_texture_unit(&primitive.pbr_material.base_color_texture, tp.albedo);
        bind_texture_unit(&primitive.pbr_material.mr_texture, tp.mr);
        bind_texture_unit(&primitive.pbr_material.normal_texture, tp.normal);
        bind_texture_unit(&primitive.pbr_material.occlusion_texture, tp.occlusion);
        bind_texture_unit(&primitive.pbr_material.emissive_texture, tp.emissive);

        if let Some(cc) = &primitive.clearcoat {
            bind_texture_unit(&cc.intensity_texture, tp.clearcoat_intensity);
            bind_texture_unit(&cc.roughness_texture, tp.clearcoat_roughness);
            bind_texture_unit(&cc.normal_texture, tp.clearcoat_normal);
        }

        unsafe {
            gl::BindTextureUnit(tp.irradiance, iblenv.irradiance_tex.id);
            gl::BindTextureUnit(tp.prefilter, iblenv.prefilter_tex.id);
            gl::BindTextureUnit(tp.brdf, self.dfg_lut.id);
        }

        Ok(())
    }

    fn set_material(&mut self, prim: &Primitive, rctx: &mut RenderCtx) {
        if rctx.app_settings.material_src == MaterialSrc::PbrOverride {
            self.material.inner = rctx.app_settings.pbr_material_override;
        } else if rctx.app_settings.material_src == MaterialSrc::Gltf {
            self.material.inner.base_color_factor = prim.pbr_material.base_color_factor;
            self.material.inner.emissive_factor[0..3]
                .copy_from_slice(&prim.pbr_material.emissive_factor);
            self.material.inner.metallic_factor = prim.pbr_material.metallic_factor;
            self.material.inner.roughness_factor = prim.pbr_material.roughness_factor;
            self.material.inner.normal_scale = prim.pbr_material.normal_scale;
            self.material.inner.occlusion_strength = prim.pbr_material.occlusion_strength;

            if let Some(intensity_factor) = prim.clearcoat.as_ref().map(|c| c.intensity_factor) {
                self.material.inner.clearcoat_intensity_factor = intensity_factor;
            }

            if let Some(roughness_factor) = prim.clearcoat.as_ref().map(|c| c.roughness_factor) {
                self.material.inner.clearcoat_roughness_factor = roughness_factor;
            }

            if let Some(normal_scale) = prim.clearcoat.as_ref().map(|c| c.normal_scale) {
                self.material.inner.clearcoat_normal_scale = normal_scale;
            }

            if let Some(anisotropy) = prim.anisotropy.as_ref().map(|a| a.anisotropy) {
                self.material.inner.anisotropy = anisotropy;
            }
        }

        self.material.update();
    }

    fn draw_mesh(prim: &Primitive) {
        unsafe {
            gl::BindVertexArray(prim.vao.id);

            gl::DrawElements(
                gl::TRIANGLES,
                prim.num_indices as i32,
                prim.indices_type,
                0 as _,
            );

            gl::BindVertexArray(0);
        };
    }

    fn render_lights(&mut self) -> Result<()> {
        let lighting = self.lighting.inner;
        let num_lights = lighting.lights;

        for (light_pos, light_color) in lighting
            .light_pos
            .iter()
            .zip(lighting.light_color)
            .take(num_lights as usize)
        {
            self.shaders.light_shader.use_shader(|| {
                self.transforms.inner.model = Mat4::from_translation(light_pos.truncate())
                    * Mat4::from_scale(Vec3::splat(0.1));
                self.transforms.update();

                self.shaders
                    .light_shader
                    .set_vec3(light_color.truncate(), cstr!("lightColor"));

                unsafe {
                    gl::BindVertexArray(self.cube.id);

                    gl::DrawElements(
                        gl::TRIANGLES,
                        cubemap::INDICES.len() as _,
                        gl::UNSIGNED_BYTE,
                        0 as _,
                    );

                    gl::BindVertexArray(0);
                };
            });
        }

        Ok(())
    }
}
