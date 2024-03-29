use std::{mem::size_of, rc::Rc};

use eyre::{eyre, Result};
use gl::types::GLenum;
use gltf::{
    image::Format,
    mesh::util::ReadIndices,
    texture::{MagFilter, MinFilter, WrappingMode},
};

use crate::ogl::{gl_buffer::GlBuffer, texture::GlTexture, vao::Vao};

mod material;
mod tangents;
mod vertex;

use self::{
    material::{Anisotropy, Clearcoat, StdPbrMaterial},
    vertex::Vertex,
};

use super::DataBundle;

/// Gltf terminology is needlessly confusing.
/// A gltf 'Mesh' contains multiple real sub-meshes (called Primitives in the gltf parlance)
pub struct Mesh {
    /// 'Primitives' of the 'mesh'
    pub primitives: Vec<Primitive>,
    /// Name of the 'Mesh'
    pub name: Option<String>,
}

impl Mesh {
    /// Create a mesh from the gltf::Mesh struct and the DataBundle
    pub fn from_gltf(mesh: &gltf::Mesh, bundle: &mut DataBundle) -> Result<Self> {
        let name = mesh.name().map(|n| n.to_owned());

        let mut primitives = Vec::new();
        for primitive in mesh.primitives() {
            let primitive = Primitive::from_gltf(&primitive, bundle)?;
            primitives.push(primitive);
        }

        Ok(Mesh { primitives, name })
    }
}

/// A Primitive represents a single 'mesh' in the normal meaning of that word
/// (a collection of vertices with a specific topology like Triangles or Lines).
pub struct Primitive {
    /// OpenGL VAO identifier
    pub vao: Vao,

    pub vertex_buffer: GlBuffer,

    pub index_buffer: GlBuffer,
    pub num_indices: usize,
    pub indices_type: GLenum,

    pub pbr_material: StdPbrMaterial,
    pub clearcoat: Option<Clearcoat>,
    pub anisotropy: Option<Anisotropy>,
}

impl Primitive {
    /// Creates the primitive from the gltf::Primitive struct and the DataBundle
    pub fn from_gltf(primitive: &gltf::Primitive, bundle: &mut DataBundle) -> Result<Self> {
        let mode = primitive.mode();
        if mode != gltf::mesh::Mode::Triangles {
            return Err(eyre!("primitive mode: '{mode:?}' is not impelemnted"));
        }

        let mut vertex_buf = Self::load_vertex_atrrib_buf(primitive, bundle)?;
        let index_buf = Self::load_indices_buf(primitive, bundle)?;

        let index_buffer = GlBuffer::new(&index_buf);

        let pbr_material = StdPbrMaterial::from_gtlf(&primitive.material(), bundle);
        let clearcoat = primitive
            .material()
            .clearcoat()
            .and_then(|cc| Clearcoat::from_gltf(&cc, bundle));

        // Placeholder until anisotropy extension is stabilized
        let anisotropy = Some(Anisotropy::new());

        if primitive.get(&gltf::Semantic::Tangents).is_none() {
            Self::check_calculate_tangents(
                &pbr_material,
                &clearcoat,
                &anisotropy,
                &mut vertex_buf,
                &index_buf,
            );
        }

        let vertex_buffer = GlBuffer::new(&vertex_buf);
        let vao = Self::create_vao(&vertex_buffer, &index_buffer);

        let prim = Self {
            vao,
            vertex_buffer,
            index_buffer,
            num_indices: index_buf.len(),
            // The type is fixed for now, maybe I'll revert it back to a flexible type in the future
            indices_type: gl::UNSIGNED_INT,
            pbr_material,
            clearcoat,
            anisotropy,
        };

        Ok(prim)
    }

    /// Creates OpenGL buffers from the loaded vertex data
    fn create_vao(vertex_buffer: &GlBuffer, index_buffer: &GlBuffer) -> Vao {
        let vao = Vao::new();

        vao.attach_index_buffer(index_buffer);
        vao.attach_vertex_buf_multiple_attribs(
            vertex_buffer,
            &Vertex::ATTRIB_SIZES,
            &Vertex::ATTRIB_INDICES,
            &Vertex::ATTRIB_TYPES,
            size_of::<Vertex>(),
            &Vertex::ATTRIB_OFFSETS,
        );

        vao
    }

    fn load_vertex_atrrib_buf(
        primitive: &gltf::Primitive,
        bundle: &DataBundle,
    ) -> Result<Vec<Vertex>> {
        let reader = primitive.reader(|buffer| Some(&bundle.buffers[buffer.index()]));

        let position_iter = reader
            .read_positions()
            .ok_or(eyre!("primitive doesn't containt positions"))?;
        let mut normals_iter = reader
            .read_normals()
            .ok_or(eyre!("primitive doesn't containt normals"))?;
        let mut tangents_iter = reader.read_tangents();

        // TODO: support multiple texture coordinates sets
        let mut texcoords_reader = None;
        let mut texture_set = 0;
        while let Some(reader) = reader.read_tex_coords(texture_set) {
            if texture_set >= 1 {
                eprintln!(
                    "WARN: primitive has more than 1 texture coordinate set: {}",
                    texture_set
                );
                texture_set += 1;
                continue;
            }

            texcoords_reader = Some(reader.into_f32());

            texture_set += 1;
        }

        let mut buf = Vec::with_capacity(position_iter.len());

        for pos in position_iter {
            let Some(normal) = normals_iter.next() else {
                return Err(eyre!("primitive attributes have different lengths"));
            };

            let texcoords = texcoords_reader
                .as_mut()
                .and_then(|t| t.next())
                .unwrap_or([0.; 2]);

            let tangent = tangents_iter
                .as_mut()
                .and_then(|t| t.next())
                .unwrap_or([0.; 4]);

            let vertex = Vertex {
                pos,
                normal,
                texcoords,
                tangent,
            };

            buf.push(vertex);
        }

        Ok(buf)
    }

    fn load_indices_buf(primitive: &gltf::Primitive, bundle: &DataBundle) -> Result<Vec<u32>> {
        let reader = primitive.reader(|buffer| Some(&bundle.buffers[buffer.index()]));

        let indices: Vec<u32> = match reader
            .read_indices()
            .ok_or(eyre!("primitive doesn't containt indices"))?
        {
            ReadIndices::U32(b) => b.collect(),
            ReadIndices::U16(b) => b.map(|i| i as u32).collect(),
            ReadIndices::U8(b) => b.map(|i| i as u32).collect(),
        };

        Ok(indices)
    }

    fn check_calculate_tangents(
        pbr_material: &StdPbrMaterial,
        clearcoat: &Option<Clearcoat>,
        anisotropy: &Option<Anisotropy>,
        vertex_buf: &mut Vec<Vertex>,
        index_buf: &[u32],
    ) {
        if pbr_material.normal_texture.is_some()
            || anisotropy.is_some()
            || clearcoat
                .as_ref()
                .map(|c| c.normal_texture.is_some())
                .unwrap_or(false)
        {
            println!("Warning: calculating tangents in a simple way");
            Self::calculate_tangents(vertex_buf, index_buf);
        }
    }
}

/// Creates a new OpenGL texture.
///
/// If the texture already exists (bundle.gl_textures\[texture_index\] == Some(...)),
/// no new texture is created, only the Texture struct is cloned.
fn create_texture(tex: &gltf::Texture, bundle: &mut DataBundle) -> Rc<GlTexture> {
    let tex_index = tex.source().index();
    if let Some(texture) = &bundle.gl_textures[tex_index] {
        return texture.clone();
    }

    let gl_tex = GlTexture::new(gl::TEXTURE_2D);
    set_texture_sampler(gl_tex.id, &tex.sampler());

    let image = &bundle.images[tex_index];

    assert!(image.width.is_power_of_two());
    assert!(image.height.is_power_of_two());

    let (internal_format, format) = match image.format {
        Format::R8 => (gl::R8, gl::RED),
        Format::R8G8 => (gl::RG8, gl::RG),
        Format::R8G8B8 => (gl::RGB8, gl::RGB),
        Format::R8G8B8A8 => (gl::RGBA8, gl::RGBA),
        f => unimplemented!("Unimplemented image format: '{f:?}'"),
    };

    let w = image.width as i32;
    let h = image.height as i32;

    let levels = 1 + f32::floor(f32::log2(i32::max(w, h) as f32)) as i32;

    unsafe {
        gl::TextureStorage2D(gl_tex.id, levels, internal_format, w, h);
        gl::TextureSubImage2D(
            gl_tex.id,
            0,
            0,
            0,
            w,
            h,
            format,
            gl::UNSIGNED_BYTE,
            image.pixels.as_ptr() as _,
        );

        gl::GenerateTextureMipmap(gl_tex.id);
    };

    let rc_tex = Rc::new(gl_tex);

    bundle.gl_textures[tex_index] = Some(rc_tex.clone());
    rc_tex
}

/// Sets the appropriate sampler functions for the currently created texture.
fn set_texture_sampler(texture: u32, sampler: &gltf::texture::Sampler) {
    let min_filter = match sampler.min_filter() {
        Some(min_filter) => match min_filter {
            MinFilter::Nearest => gl::NEAREST,
            MinFilter::Linear => gl::LINEAR,
            MinFilter::NearestMipmapNearest => gl::NEAREST_MIPMAP_NEAREST,
            MinFilter::LinearMipmapNearest => gl::LINEAR_MIPMAP_NEAREST,
            MinFilter::NearestMipmapLinear => gl::NEAREST_MIPMAP_LINEAR,
            MinFilter::LinearMipmapLinear => gl::LINEAR_MIPMAP_LINEAR,
        },
        None => gl::LINEAR_MIPMAP_LINEAR,
    };

    let mag_filter = match sampler.mag_filter() {
        Some(mag_filter) => match mag_filter {
            MagFilter::Nearest => gl::NEAREST,
            MagFilter::Linear => gl::LINEAR,
        },
        None => gl::LINEAR,
    };

    unsafe {
        gl::TextureParameteri(texture, gl::TEXTURE_MIN_FILTER, min_filter as i32);
        gl::TextureParameteri(texture, gl::TEXTURE_MAG_FILTER, mag_filter as i32);
    }

    let wrap_s = match sampler.wrap_s() {
        WrappingMode::ClampToEdge => gl::CLAMP_TO_EDGE,
        WrappingMode::MirroredRepeat => gl::MIRRORED_REPEAT,
        WrappingMode::Repeat => gl::REPEAT,
    };

    let wrap_t = match sampler.wrap_t() {
        WrappingMode::ClampToEdge => gl::CLAMP_TO_EDGE,
        WrappingMode::MirroredRepeat => gl::MIRRORED_REPEAT,
        WrappingMode::Repeat => gl::REPEAT,
    };

    unsafe {
        gl::TextureParameteri(texture, gl::TEXTURE_WRAP_S, wrap_s as i32);
        gl::TextureParameteri(texture, gl::TEXTURE_WRAP_T, wrap_t as i32);
    }
}
