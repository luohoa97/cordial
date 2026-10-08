//! Render at a fraction of the window and upscale in the present path
//! (`CORDIAL_RENDER_SCALE`, a prototype; docs/adr/ADR-057).
//!
//! The engine builds its swapchain from the extent `vkGetPhysicalDeviceSurface
//! CapabilitiesKHR` reports and renders straight into the swapchain's images,
//! so the smallest correct place to change what it renders at is exactly that
//! boundary, the same kind of answer ADR-049 gives for ETC2 and the same
//! reading of ADR-001: Cordial answers a call the engine made to a function
//! pointer Cordial handed it, and nothing in the engine's memory or code is
//! touched.
//!
//! * `vkGetPhysicalDeviceSurfaceCapabilitiesKHR` reports the extent scaled
//!   (`vulkan.rs`), and this module remembers the real one per surface.
//! * `vkCreateSwapchainKHR` builds the **real** swapchain at the window's size
//!   and widens its usage so a pass can draw into it and a screenshot can read
//!   it, then [`attach`] creates one **proxy** image per real image at the
//!   engine's size, which is what the engine is given.
//! * `vkGetSwapchainImagesKHR` returns the proxies. Acquire is untouched: index
//!   `i` of the proxies is index `i` of the real images.
//! * `vkQueuePresentKHR` first submits a pre-recorded pass that samples proxy
//!   `i` and draws into real image `i`, waiting on the semaphores the engine's
//!   present would have waited on, and presents waiting on the pass's own.
//!
//! Costs: one image the size of the render target per swapchain image, one
//! queue submission and one full-screen draw per frame, one semaphore per
//! image, and nothing at all when the variable is unset.
//!
//! **Known deviation from the specification.** The engine ends each frame
//! with its image in `VK_IMAGE_LAYOUT_PRESENT_SRC_KHR`, which the
//! specification allows only for a presentable image, and the proxies are not
//! presentable. The pass treats the proxy as being in that layout and puts it
//! back, which is what the engine believes. Mesa treats the layout as an
//! ordinary one; whether another driver does has not been run.
//!
//! Hand-laid structures, for the reason `capture.rs` gives. Their sizes are
//! asserted against the C compiler's in `tests`.

use std::collections::HashMap;
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const VK_SUCCESS: i32 = 0;
const VK_INCOMPLETE: i32 = 5;
const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;

/// The variable that asks for it, and the one that picks the filter.
pub const SCALE_ENV: &str = "CORDIAL_RENDER_SCALE";
pub const FILTER_ENV: &str = "CORDIAL_RENDER_SCALE_FILTER";

/// The lowest scale accepted. Below a quarter of the window the engine's own
/// layout starts to fail for reasons unrelated to this module.
const MIN_SCALE: f32 = 0.25;

/// `CORDIAL_RENDER_SCALE`, read once. `None` is "off", and is what an unset,
/// unparsable or 1.0-and-above value means: the prototype is off by default and
/// a typo must not quietly turn it on.
pub fn scale() -> Option<f32> {
    static S: OnceLock<Option<f32>> = OnceLock::new();
    *S.get_or_init(|| {
        let raw = std::env::var(SCALE_ENV).ok()?;
        let r = parse_scale(&raw);
        match r {
            Some(s) => println!("[android] render-scale: {SCALE_ENV}={raw} -> rendering at {:.0}% and upscaling", s * 100.0),
            None => println!("[android] render-scale: {SCALE_ENV}={raw:?} ignored (want {MIN_SCALE}..0.99); rendering at the window's size"),
        }
        r
    })
}

fn parse_scale(text: &str) -> Option<f32> {
    let v: f32 = text.trim().parse().ok()?;
    (v.is_finite() && (MIN_SCALE..1.0).contains(&v)).then_some(v)
}

/// The engine's extent for a real one.
pub fn scaled(real: (u32, u32), scale: f32) -> (u32, u32) {
    let f = |v: u32| ((v as f32 * scale).round() as u32).max(1);
    (f(real.0), f(real.1))
}

/// A pointer position in window pixels, as the engine must be told it. The
/// engine lays its interface out against the extent it was reported, so a
/// click at window pixel `x` is at `x * scale` as far as it is concerned.
/// Identity when the prototype is off. Touch contacts are not mapped (untested).
pub fn to_engine(x: f32, y: f32) -> (f32, f32) {
    match scale() {
        Some(s) => (x * s, y * s),
        None => (x, y),
    }
}

/// A length the engine reported (a text box's rectangle, its font size), in
/// window pixels: the inverse of [`to_engine`].
pub fn to_window(v: f32) -> f32 {
    match scale() {
        Some(s) => v / s,
        None => v,
    }
}

/// 0 is bilinear, 1 is Snapdragon GSR 1 (the default).
fn filter_mode() -> u32 {
    static M: OnceLock<u32> = OnceLock::new();
    *M.get_or_init(|| match std::env::var(FILTER_ENV).ok().as_deref() {
        Some("bilinear") => 0,
        _ => 1,
    })
}

/// The real extent of each surface, as last reported to the engine, so the
/// swapchain built from the engine's scaled request can be built at the real one.
static REAL_EXTENTS: Mutex<Option<HashMap<u64, (u32, u32)>>> = Mutex::new(None);

pub fn note_real_extent(surface: u64, real: (u32, u32)) {
    REAL_EXTENTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(surface, real);
}

/// The real extent for a swapchain the engine asked to build at `engine`.
/// What was last reported is used if it scales to what was asked for; otherwise
/// the request is divided back, which is off by at most a pixel.
pub fn real_extent_for(surface: u64, engine: (u32, u32), scale: f32) -> (u32, u32) {
    let noted = REAL_EXTENTS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&surface).copied());
    match noted {
        Some(real) if scaled(real, scale) == engine => real,
        _ => ((engine.0 as f32 / scale).round() as u32, (engine.1 as f32 / scale).round() as u32),
    }
}

// --------------------------------------------------------------- constants
const ST_SUBMIT_INFO: u32 = 4;
const ST_MEMORY_ALLOCATE_INFO: u32 = 5;
const ST_SEMAPHORE_CREATE_INFO: u32 = 9;
const ST_QUERY_POOL_CREATE_INFO: u32 = 11;
const ST_IMAGE_CREATE_INFO: u32 = 14;
const ST_IMAGE_VIEW_CREATE_INFO: u32 = 15;
const ST_SHADER_MODULE_CREATE_INFO: u32 = 16;
const ST_PIPELINE_SHADER_STAGE: u32 = 18;
const ST_PIPELINE_VERTEX_INPUT: u32 = 19;
const ST_PIPELINE_INPUT_ASSEMBLY: u32 = 20;
const ST_PIPELINE_VIEWPORT: u32 = 22;
const ST_PIPELINE_RASTERIZATION: u32 = 23;
const ST_PIPELINE_MULTISAMPLE: u32 = 24;
const ST_PIPELINE_COLOR_BLEND: u32 = 26;
const ST_GRAPHICS_PIPELINE: u32 = 28;
const ST_PIPELINE_LAYOUT: u32 = 30;
const ST_SAMPLER_CREATE_INFO: u32 = 31;
const ST_DESCRIPTOR_SET_LAYOUT: u32 = 32;
const ST_DESCRIPTOR_POOL: u32 = 33;
const ST_DESCRIPTOR_SET_ALLOCATE: u32 = 34;
const ST_WRITE_DESCRIPTOR_SET: u32 = 35;
const ST_FRAMEBUFFER: u32 = 37;
const ST_RENDER_PASS: u32 = 38;
const ST_COMMAND_POOL: u32 = 39;
const ST_COMMAND_BUFFER_ALLOCATE: u32 = 40;
const ST_COMMAND_BUFFER_BEGIN: u32 = 42;
const ST_RENDER_PASS_BEGIN: u32 = 43;
const ST_IMAGE_MEMORY_BARRIER: u32 = 45;

const LAYOUT_UNDEFINED: u32 = 0;
const LAYOUT_COLOR_ATTACHMENT: u32 = 2;
const LAYOUT_SHADER_READ_ONLY: u32 = 5;
const LAYOUT_PRESENT_SRC: u32 = 1_000_001_002;

pub const USAGE_TRANSFER_SRC: u32 = 0x1;
pub const USAGE_SAMPLED: u32 = 0x4;
pub const USAGE_COLOR_ATTACHMENT: u32 = 0x10;

const STAGE_TOP_OF_PIPE: u32 = 0x1;
const STAGE_FRAGMENT_SHADER: u32 = 0x80;
const STAGE_COLOR_ATTACHMENT_OUTPUT: u32 = 0x400;
const STAGE_BOTTOM_OF_PIPE: u32 = 0x2000;
const STAGE_ALL_COMMANDS: u32 = 0x10000;
const ACCESS_SHADER_READ: u32 = 0x20;
const ACCESS_COLOR_ATTACHMENT_WRITE: u32 = 0x100;
const ACCESS_MEMORY_READ: u32 = 0x8000;
const ACCESS_MEMORY_WRITE: u32 = 0x10000;
const SUBPASS_EXTERNAL: u32 = u32::MAX;
const MEMORY_DEVICE_LOCAL: u32 = 1;
/// `VK_SWAPCHAIN_CREATE_MUTABLE_FORMAT_BIT_KHR` and the image flag it implies.
const SWAPCHAIN_MUTABLE_FORMAT: u32 = 0x4;
const IMAGE_MUTABLE_FORMAT: u32 = 0x8;

static VERT: &[u8] = include_bytes!("../../shaders/render_scale/upscale.vert.spv");
static FRAG: &[u8] = include_bytes!("../../shaders/render_scale/upscale.frag.spv");

// ----------------------------------------------------------------- structs
type P = *const c_void;

#[repr(C)]
struct ImageCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    image_type: u32,
    format: u32,
    extent: [u32; 3],
    mip_levels: u32,
    array_layers: u32,
    samples: u32,
    tiling: u32,
    usage: u32,
    sharing_mode: u32,
    queue_family_index_count: u32,
    queue_family_indices: *const u32,
    initial_layout: u32,
}

#[repr(C)]
#[derive(Default)]
struct MemoryRequirements {
    size: u64,
    alignment: u64,
    memory_type_bits: u32,
}

#[repr(C)]
struct MemoryAllocateInfo {
    s_type: u32,
    next: P,
    size: u64,
    memory_type_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SubresourceRange {
    aspect_mask: u32,
    base_mip_level: u32,
    level_count: u32,
    base_array_layer: u32,
    layer_count: u32,
}
const COLOR_RANGE: SubresourceRange =
    SubresourceRange { aspect_mask: 1, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 };

#[repr(C)]
struct ImageViewCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    image: u64,
    view_type: u32,
    format: u32,
    components: [u32; 4],
    range: SubresourceRange,
}

#[repr(C)]
struct SamplerCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    mag_filter: u32,
    min_filter: u32,
    mipmap_mode: u32,
    address_u: u32,
    address_v: u32,
    address_w: u32,
    mip_lod_bias: f32,
    anisotropy_enable: u32,
    max_anisotropy: f32,
    compare_enable: u32,
    compare_op: u32,
    min_lod: f32,
    max_lod: f32,
    border_color: u32,
    unnormalized: u32,
}

#[repr(C)]
struct ShaderModuleCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    code_size: usize,
    code: *const u32,
}

#[repr(C)]
struct DescriptorSetLayoutBinding {
    binding: u32,
    descriptor_type: u32,
    count: u32,
    stage_flags: u32,
    immutable_samplers: P,
}

#[repr(C)]
struct DescriptorSetLayoutCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    binding_count: u32,
    bindings: *const DescriptorSetLayoutBinding,
}

#[repr(C)]
struct PushConstantRange {
    stage_flags: u32,
    offset: u32,
    size: u32,
}

#[repr(C)]
struct PipelineLayoutCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    set_layout_count: u32,
    set_layouts: *const u64,
    push_constant_range_count: u32,
    push_constant_ranges: *const PushConstantRange,
}

#[repr(C)]
struct DescriptorPoolSize {
    descriptor_type: u32,
    count: u32,
}

#[repr(C)]
struct DescriptorPoolCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    max_sets: u32,
    pool_size_count: u32,
    pool_sizes: *const DescriptorPoolSize,
}

#[repr(C)]
struct DescriptorSetAllocateInfo {
    s_type: u32,
    next: P,
    pool: u64,
    count: u32,
    layouts: *const u64,
}

#[repr(C)]
struct DescriptorImageInfo {
    sampler: u64,
    image_view: u64,
    layout: u32,
}

#[repr(C)]
struct WriteDescriptorSet {
    s_type: u32,
    next: P,
    set: u64,
    binding: u32,
    array_element: u32,
    count: u32,
    descriptor_type: u32,
    image_info: *const DescriptorImageInfo,
    buffer_info: P,
    texel_buffer_view: P,
}

#[repr(C)]
struct AttachmentDescription {
    flags: u32,
    format: u32,
    samples: u32,
    load_op: u32,
    store_op: u32,
    stencil_load_op: u32,
    stencil_store_op: u32,
    initial_layout: u32,
    final_layout: u32,
}

#[repr(C)]
struct AttachmentReference {
    attachment: u32,
    layout: u32,
}

#[repr(C)]
struct SubpassDescription {
    flags: u32,
    pipeline_bind_point: u32,
    input_count: u32,
    inputs: P,
    color_count: u32,
    colors: *const AttachmentReference,
    resolves: P,
    depth: P,
    preserve_count: u32,
    preserves: P,
}

#[repr(C)]
struct SubpassDependency {
    src_subpass: u32,
    dst_subpass: u32,
    src_stage: u32,
    dst_stage: u32,
    src_access: u32,
    dst_access: u32,
    dependency_flags: u32,
}

#[repr(C)]
struct RenderPassCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    attachment_count: u32,
    attachments: *const AttachmentDescription,
    subpass_count: u32,
    subpasses: *const SubpassDescription,
    dependency_count: u32,
    dependencies: *const SubpassDependency,
}

#[repr(C)]
struct FramebufferCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    render_pass: u64,
    attachment_count: u32,
    attachments: *const u64,
    width: u32,
    height: u32,
    layers: u32,
}

#[repr(C)]
struct PipelineShaderStage {
    s_type: u32,
    next: P,
    flags: u32,
    stage: u32,
    module: u64,
    name: *const c_char,
    specialization: P,
}

#[repr(C)]
struct PipelineVertexInput {
    s_type: u32,
    next: P,
    flags: u32,
    binding_count: u32,
    bindings: P,
    attribute_count: u32,
    attributes: P,
}

#[repr(C)]
struct PipelineInputAssembly {
    s_type: u32,
    next: P,
    flags: u32,
    topology: u32,
    primitive_restart: u32,
}

#[repr(C)]
struct Viewport {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    min_depth: f32,
    max_depth: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect2D {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[repr(C)]
struct PipelineViewport {
    s_type: u32,
    next: P,
    flags: u32,
    viewport_count: u32,
    viewports: *const Viewport,
    scissor_count: u32,
    scissors: *const Rect2D,
}

#[repr(C)]
struct PipelineRasterization {
    s_type: u32,
    next: P,
    flags: u32,
    depth_clamp: u32,
    rasterizer_discard: u32,
    polygon_mode: u32,
    cull_mode: u32,
    front_face: u32,
    depth_bias: u32,
    depth_bias_constant: f32,
    depth_bias_clamp: f32,
    depth_bias_slope: f32,
    line_width: f32,
}

#[repr(C)]
struct PipelineMultisample {
    s_type: u32,
    next: P,
    flags: u32,
    samples: u32,
    sample_shading: u32,
    min_sample_shading: f32,
    sample_mask: P,
    alpha_to_coverage: u32,
    alpha_to_one: u32,
}

#[repr(C)]
struct PipelineColorBlendAttachment {
    blend_enable: u32,
    src_color: u32,
    dst_color: u32,
    color_op: u32,
    src_alpha: u32,
    dst_alpha: u32,
    alpha_op: u32,
    write_mask: u32,
}

#[repr(C)]
struct PipelineColorBlend {
    s_type: u32,
    next: P,
    flags: u32,
    logic_op_enable: u32,
    logic_op: u32,
    attachment_count: u32,
    attachments: *const PipelineColorBlendAttachment,
    blend_constants: [f32; 4],
}

#[repr(C)]
struct GraphicsPipelineCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    stage_count: u32,
    stages: *const PipelineShaderStage,
    vertex_input: *const PipelineVertexInput,
    input_assembly: *const PipelineInputAssembly,
    tessellation: P,
    viewport: *const PipelineViewport,
    rasterization: *const PipelineRasterization,
    multisample: *const PipelineMultisample,
    depth_stencil: P,
    color_blend: *const PipelineColorBlend,
    dynamic: P,
    layout: u64,
    render_pass: u64,
    subpass: u32,
    base_pipeline: u64,
    base_pipeline_index: i32,
}

#[repr(C)]
struct CommandPoolCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    queue_family_index: u32,
}

#[repr(C)]
struct CommandBufferAllocateInfo {
    s_type: u32,
    next: P,
    pool: u64,
    level: u32,
    count: u32,
}

#[repr(C)]
struct CommandBufferBeginInfo {
    s_type: u32,
    next: P,
    flags: u32,
    inheritance: P,
}

#[repr(C)]
struct RenderPassBeginInfo {
    s_type: u32,
    next: P,
    render_pass: u64,
    framebuffer: u64,
    render_area: Rect2D,
    clear_count: u32,
    clears: P,
}

#[repr(C)]
struct ImageMemoryBarrier {
    s_type: u32,
    next: P,
    src_access: u32,
    dst_access: u32,
    old_layout: u32,
    new_layout: u32,
    src_family: u32,
    dst_family: u32,
    image: u64,
    range: SubresourceRange,
}

#[repr(C)]
struct SemaphoreCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
}

#[repr(C)]
struct QueryPoolCreateInfo {
    s_type: u32,
    next: P,
    flags: u32,
    query_type: u32,
    query_count: u32,
    pipeline_statistics: u32,
}

#[repr(C)]
struct SubmitInfo {
    s_type: u32,
    next: P,
    wait_count: u32,
    waits: *const u64,
    wait_stages: *const u32,
    command_buffer_count: u32,
    command_buffers: *const u64,
    signal_count: u32,
    signals: *const u64,
}

#[repr(C)]
struct QueueFamilyProperties {
    queue_flags: u32,
    queue_count: u32,
    timestamp_valid_bits: u32,
    granularity: [u32; 3],
}

// --------------------------------------------------------------- functions
type Gdpa = extern "C" fn(u64, *const c_char) -> *mut c_void;

macro_rules! load {
    ($get:expr, $dev:expr, $name:literal) => {{
        let n = concat!($name, "\0");
        let p = $get($dev, n.as_ptr() as *const c_char);
        if p.is_null() {
            return Err(concat!("the driver has no ", $name).to_string());
        }
        // SAFETY: the loader returned a function for exactly this name, whose
        // signature is the one the Vulkan specification gives it.
        unsafe { std::mem::transmute(p) }
    }};
}

type Create<T> = extern "C" fn(u64, *const T, P, *mut u64) -> i32;
type Destroy = extern "C" fn(u64, u64, P);

struct Fns {
    get_swapchain_images: extern "C" fn(u64, u64, *mut u32, *mut u64) -> i32,
    create_image: Create<ImageCreateInfo>,
    get_image_memory_requirements: extern "C" fn(u64, u64, *mut MemoryRequirements),
    allocate_memory: Create<MemoryAllocateInfo>,
    bind_image_memory: extern "C" fn(u64, u64, u64, u64) -> i32,
    create_image_view: Create<ImageViewCreateInfo>,
    create_sampler: Create<SamplerCreateInfo>,
    create_shader_module: Create<ShaderModuleCreateInfo>,
    create_descriptor_set_layout: Create<DescriptorSetLayoutCreateInfo>,
    create_pipeline_layout: Create<PipelineLayoutCreateInfo>,
    create_descriptor_pool: Create<DescriptorPoolCreateInfo>,
    allocate_descriptor_sets: extern "C" fn(u64, *const DescriptorSetAllocateInfo, *mut u64) -> i32,
    update_descriptor_sets: extern "C" fn(u64, u32, *const WriteDescriptorSet, u32, P),
    create_render_pass: Create<RenderPassCreateInfo>,
    create_framebuffer: Create<FramebufferCreateInfo>,
    create_graphics_pipelines: extern "C" fn(u64, u64, u32, *const GraphicsPipelineCreateInfo, P, *mut u64) -> i32,
    create_command_pool: Create<CommandPoolCreateInfo>,
    allocate_command_buffers: extern "C" fn(u64, *const CommandBufferAllocateInfo, *mut u64) -> i32,
    begin_command_buffer: extern "C" fn(u64, *const CommandBufferBeginInfo) -> i32,
    end_command_buffer: extern "C" fn(u64) -> i32,
    cmd_pipeline_barrier: extern "C" fn(u64, u32, u32, u32, u32, P, u32, P, u32, *const ImageMemoryBarrier),
    cmd_begin_render_pass: extern "C" fn(u64, *const RenderPassBeginInfo, u32),
    cmd_end_render_pass: extern "C" fn(u64),
    cmd_bind_pipeline: extern "C" fn(u64, u32, u64),
    cmd_bind_descriptor_sets: extern "C" fn(u64, u32, u64, u32, u32, *const u64, u32, *const u32),
    cmd_push_constants: extern "C" fn(u64, u64, u32, u32, u32, *const c_void),
    cmd_draw: extern "C" fn(u64, u32, u32, u32, u32),
    create_semaphore: Create<SemaphoreCreateInfo>,
    queue_submit: extern "C" fn(u64, u32, *const SubmitInfo, u64) -> i32,
    create_query_pool: Create<QueryPoolCreateInfo>,
    cmd_reset_query_pool: extern "C" fn(u64, u64, u32, u32),
    cmd_write_timestamp: extern "C" fn(u64, u32, u64, u32),
    get_query_pool_results: extern "C" fn(u64, u64, u32, u32, usize, *mut c_void, u64, u32) -> i32,
    device_wait_idle: extern "C" fn(u64) -> i32,
    destroy_image: Destroy,
    destroy_image_view: Destroy,
    destroy_sampler: Destroy,
    destroy_shader_module: Destroy,
    destroy_descriptor_set_layout: Destroy,
    destroy_pipeline_layout: Destroy,
    destroy_descriptor_pool: Destroy,
    destroy_render_pass: Destroy,
    destroy_framebuffer: Destroy,
    destroy_pipeline: Destroy,
    destroy_command_pool: Destroy,
    destroy_semaphore: Destroy,
    destroy_query_pool: Destroy,
    free_memory: Destroy,
}

fn load_fns(gdpa: Gdpa, d: u64) -> Result<Fns, String> {
    Ok(Fns {
        get_swapchain_images: load!(gdpa, d, "vkGetSwapchainImagesKHR"),
        create_image: load!(gdpa, d, "vkCreateImage"),
        get_image_memory_requirements: load!(gdpa, d, "vkGetImageMemoryRequirements"),
        allocate_memory: load!(gdpa, d, "vkAllocateMemory"),
        bind_image_memory: load!(gdpa, d, "vkBindImageMemory"),
        create_image_view: load!(gdpa, d, "vkCreateImageView"),
        create_sampler: load!(gdpa, d, "vkCreateSampler"),
        create_shader_module: load!(gdpa, d, "vkCreateShaderModule"),
        create_descriptor_set_layout: load!(gdpa, d, "vkCreateDescriptorSetLayout"),
        create_pipeline_layout: load!(gdpa, d, "vkCreatePipelineLayout"),
        create_descriptor_pool: load!(gdpa, d, "vkCreateDescriptorPool"),
        allocate_descriptor_sets: load!(gdpa, d, "vkAllocateDescriptorSets"),
        update_descriptor_sets: load!(gdpa, d, "vkUpdateDescriptorSets"),
        create_render_pass: load!(gdpa, d, "vkCreateRenderPass"),
        create_framebuffer: load!(gdpa, d, "vkCreateFramebuffer"),
        create_graphics_pipelines: load!(gdpa, d, "vkCreateGraphicsPipelines"),
        create_command_pool: load!(gdpa, d, "vkCreateCommandPool"),
        allocate_command_buffers: load!(gdpa, d, "vkAllocateCommandBuffers"),
        begin_command_buffer: load!(gdpa, d, "vkBeginCommandBuffer"),
        end_command_buffer: load!(gdpa, d, "vkEndCommandBuffer"),
        cmd_pipeline_barrier: load!(gdpa, d, "vkCmdPipelineBarrier"),
        cmd_begin_render_pass: load!(gdpa, d, "vkCmdBeginRenderPass"),
        cmd_end_render_pass: load!(gdpa, d, "vkCmdEndRenderPass"),
        cmd_bind_pipeline: load!(gdpa, d, "vkCmdBindPipeline"),
        cmd_bind_descriptor_sets: load!(gdpa, d, "vkCmdBindDescriptorSets"),
        cmd_push_constants: load!(gdpa, d, "vkCmdPushConstants"),
        cmd_draw: load!(gdpa, d, "vkCmdDraw"),
        create_semaphore: load!(gdpa, d, "vkCreateSemaphore"),
        queue_submit: load!(gdpa, d, "vkQueueSubmit"),
        create_query_pool: load!(gdpa, d, "vkCreateQueryPool"),
        cmd_reset_query_pool: load!(gdpa, d, "vkCmdResetQueryPool"),
        cmd_write_timestamp: load!(gdpa, d, "vkCmdWriteTimestamp"),
        get_query_pool_results: load!(gdpa, d, "vkGetQueryPoolResults"),
        device_wait_idle: load!(gdpa, d, "vkDeviceWaitIdle"),
        destroy_image: load!(gdpa, d, "vkDestroyImage"),
        destroy_image_view: load!(gdpa, d, "vkDestroyImageView"),
        destroy_sampler: load!(gdpa, d, "vkDestroySampler"),
        destroy_shader_module: load!(gdpa, d, "vkDestroyShaderModule"),
        destroy_descriptor_set_layout: load!(gdpa, d, "vkDestroyDescriptorSetLayout"),
        destroy_pipeline_layout: load!(gdpa, d, "vkDestroyPipelineLayout"),
        destroy_descriptor_pool: load!(gdpa, d, "vkDestroyDescriptorPool"),
        destroy_render_pass: load!(gdpa, d, "vkDestroyRenderPass"),
        destroy_framebuffer: load!(gdpa, d, "vkDestroyFramebuffer"),
        destroy_pipeline: load!(gdpa, d, "vkDestroyPipeline"),
        destroy_command_pool: load!(gdpa, d, "vkDestroyCommandPool"),
        destroy_semaphore: load!(gdpa, d, "vkDestroySemaphore"),
        destroy_query_pool: load!(gdpa, d, "vkDestroyQueryPool"),
        free_memory: load!(gdpa, d, "vkFreeMemory"),
    })
}

// ------------------------------------------------------------------- state

/// What the engine asked for, as far as the proxies need it.
pub struct EngineRequest {
    pub format: u32,
    pub usage: u32,
    pub flags: u32,
    pub extent: (u32, u32),
}

struct Chain {
    device: u64,
    fns: Fns,
    real_extent: (u32, u32),
    proxies: Vec<u64>,
    memories: Vec<u64>,
    proxy_views: Vec<u64>,
    real_views: Vec<u64>,
    framebuffers: Vec<u64>,
    semaphores: Vec<u64>,
    cmds: Vec<u64>,
    used: Vec<bool>,
    sampler: u64,
    shaders: [u64; 2],
    set_layout: u64,
    pipeline_layout: u64,
    pool: u64,
    render_pass: u64,
    pipeline: u64,
    cmd_pool: u64,
    query_pool: u64,
    ts_mask: u64,
    ts_period_ns: f64,
    /// Sum of the pass's GPU time, and how many frames it covers.
    gpu_ns: f64,
    gpu_frames: u32,
    frames: u64,
}

static CHAINS: Mutex<Option<HashMap<u64, Chain>>> = Mutex::new(None);
/// Total frames upscaled, for `info`-style reporting.
pub static UPSCALED: AtomicU64 = AtomicU64::new(0);

fn check(what: &str, rc: i32) -> Result<(), String> {
    if rc == VK_SUCCESS { Ok(()) } else { Err(format!("{what} returned {rc}")) }
}

fn mem_type(props: &super::capture::PhysicalDeviceMemoryProperties, bits: u32, want: u32) -> Option<u32> {
    (0..props.memory_type_count.min(32)).find(|&i| bits & (1 << i) != 0 && props.memory_types[i as usize].property_flags & want == want)
}

/// Build the proxies and the pass for a swapchain the engine just created.
///
/// `real` is the swapchain handle both sides use. On error nothing is left
/// registered and the caller must fail the creation: handing the engine a
/// swapchain whose images are the wrong size is worse than refusing.
pub fn attach(
    device: u64,
    swapchain: u64,
    req: &EngineRequest,
    real_extent: (u32, u32),
    queue_family: u32,
    gdpa: Gdpa,
    physical_device: usize,
) -> Result<(), String> {
    let fns = load_fns(gdpa, device)?;
    let mut chain = Chain {
        device,
        fns,
        real_extent,
        proxies: vec![],
        memories: vec![],
        proxy_views: vec![],
        real_views: vec![],
        framebuffers: vec![],
        semaphores: vec![],
        cmds: vec![],
        used: vec![],
        sampler: 0,
        shaders: [0; 2],
        set_layout: 0,
        pipeline_layout: 0,
        pool: 0,
        render_pass: 0,
        pipeline: 0,
        cmd_pool: 0,
        query_pool: 0,
        ts_mask: 0,
        ts_period_ns: 0.0,
        gpu_ns: 0.0,
        gpu_frames: 0,
        frames: 0,
    };
    match build(&mut chain, swapchain, req, queue_family, physical_device) {
        Ok(()) => {
            CHAINS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(swapchain, chain);
            Ok(())
        }
        Err(e) => {
            teardown(&chain);
            Err(e)
        }
    }
}

fn build(c: &mut Chain, swapchain: u64, req: &EngineRequest, queue_family: u32, physical_device: usize) -> Result<(), String> {
    let d = c.device;
    // The real images.
    let mut n = 0u32;
    check("vkGetSwapchainImagesKHR", (c.fns.get_swapchain_images)(d, swapchain, &mut n, std::ptr::null_mut()))?;
    let mut real = vec![0u64; n as usize];
    check("vkGetSwapchainImagesKHR", (c.fns.get_swapchain_images)(d, swapchain, &mut n, real.as_mut_ptr()))?;
    if real.is_empty() {
        return Err("the swapchain has no images".into());
    }

    // Memory types.
    let gpdmp = super::vulkan::host_instance_proc(c"vkGetPhysicalDeviceMemoryProperties");
    if gpdmp.is_null() {
        return Err("no vkGetPhysicalDeviceMemoryProperties".into());
    }
    // SAFETY: the host's function for exactly this name.
    let gpdmp: extern "C" fn(usize, *mut super::capture::PhysicalDeviceMemoryProperties) = unsafe { std::mem::transmute(gpdmp) };
    // SAFETY: a plain-data out structure the driver fills completely.
    let mut props: super::capture::PhysicalDeviceMemoryProperties = unsafe { std::mem::zeroed() };
    gpdmp(physical_device, &mut props);

    // Timestamps: the period from `VkPhysicalDeviceProperties::limits`, the
    // valid bits from the queue family. Absent either, the pass is untimed.
    let (period, valid_bits) = timestamp_facts(physical_device, queue_family);
    c.ts_period_ns = period;
    c.ts_mask = match valid_bits {
        0 => 0,
        64.. => u64::MAX,
        b => (1u64 << b) - 1,
    };

    let proxy_flags = if req.flags & SWAPCHAIN_MUTABLE_FORMAT != 0 { IMAGE_MUTABLE_FORMAT } else { 0 };
    for &r in &real {
        let ci = ImageCreateInfo {
            s_type: ST_IMAGE_CREATE_INFO,
            next: std::ptr::null(),
            flags: proxy_flags,
            image_type: 1,
            format: req.format,
            extent: [req.extent.0, req.extent.1, 1],
            mip_levels: 1,
            array_layers: 1,
            samples: 1,
            tiling: 0,
            usage: req.usage | USAGE_SAMPLED | USAGE_TRANSFER_SRC,
            sharing_mode: 0,
            queue_family_index_count: 0,
            queue_family_indices: std::ptr::null(),
            initial_layout: LAYOUT_UNDEFINED,
        };
        let mut img = 0u64;
        check("vkCreateImage", (c.fns.create_image)(d, &ci, std::ptr::null(), &mut img))?;
        c.proxies.push(img);
        let mut mr = MemoryRequirements::default();
        (c.fns.get_image_memory_requirements)(d, img, &mut mr);
        let ty = mem_type(&props, mr.memory_type_bits, MEMORY_DEVICE_LOCAL)
            .or_else(|| mem_type(&props, mr.memory_type_bits, 0))
            .ok_or("no memory type for the proxy image")?;
        let ai = MemoryAllocateInfo { s_type: ST_MEMORY_ALLOCATE_INFO, next: std::ptr::null(), size: mr.size, memory_type_index: ty };
        let mut mem = 0u64;
        check("vkAllocateMemory", (c.fns.allocate_memory)(d, &ai, std::ptr::null(), &mut mem))?;
        c.memories.push(mem);
        check("vkBindImageMemory", (c.fns.bind_image_memory)(d, img, mem, 0))?;
        let view = |image: u64, fns: &Fns| -> Result<u64, String> {
            let vi = ImageViewCreateInfo {
                s_type: ST_IMAGE_VIEW_CREATE_INFO,
                next: std::ptr::null(),
                flags: 0,
                image,
                view_type: 1,
                format: req.format,
                components: [0; 4],
                range: COLOR_RANGE,
            };
            let mut v = 0u64;
            check("vkCreateImageView", (fns.create_image_view)(d, &vi, std::ptr::null(), &mut v))?;
            Ok(v)
        };
        let pv = view(img, &c.fns)?;
        c.proxy_views.push(pv);
        let rv = view(r, &c.fns)?;
        c.real_views.push(rv);
    }

    // Sampler: linear, clamped. GSR also relies on the base fetch being filtered.
    let si = SamplerCreateInfo {
        s_type: ST_SAMPLER_CREATE_INFO,
        next: std::ptr::null(),
        flags: 0,
        mag_filter: 1,
        min_filter: 1,
        mipmap_mode: 0,
        address_u: 2,
        address_v: 2,
        address_w: 2,
        mip_lod_bias: 0.0,
        anisotropy_enable: 0,
        max_anisotropy: 1.0,
        compare_enable: 0,
        compare_op: 0,
        min_lod: 0.0,
        max_lod: 0.0,
        border_color: 0,
        unnormalized: 0,
    };
    check("vkCreateSampler", (c.fns.create_sampler)(d, &si, std::ptr::null(), &mut c.sampler))?;

    for (i, code) in [VERT, FRAG].into_iter().enumerate() {
        let mi = ShaderModuleCreateInfo {
            s_type: ST_SHADER_MODULE_CREATE_INFO,
            next: std::ptr::null(),
            flags: 0,
            code_size: code.len(),
            code: code.as_ptr() as *const u32,
        };
        check("vkCreateShaderModule", (c.fns.create_shader_module)(d, &mi, std::ptr::null(), &mut c.shaders[i]))?;
    }

    let binding = DescriptorSetLayoutBinding { binding: 0, descriptor_type: 1, count: 1, stage_flags: 0x10, immutable_samplers: std::ptr::null() };
    let li = DescriptorSetLayoutCreateInfo { s_type: ST_DESCRIPTOR_SET_LAYOUT, next: std::ptr::null(), flags: 0, binding_count: 1, bindings: &binding };
    check("vkCreateDescriptorSetLayout", (c.fns.create_descriptor_set_layout)(d, &li, std::ptr::null(), &mut c.set_layout))?;
    let range = PushConstantRange { stage_flags: 0x10, offset: 0, size: 32 };
    let pli = PipelineLayoutCreateInfo {
        s_type: ST_PIPELINE_LAYOUT,
        next: std::ptr::null(),
        flags: 0,
        set_layout_count: 1,
        set_layouts: &c.set_layout,
        push_constant_range_count: 1,
        push_constant_ranges: &range,
    };
    check("vkCreatePipelineLayout", (c.fns.create_pipeline_layout)(d, &pli, std::ptr::null(), &mut c.pipeline_layout))?;

    let count = real.len() as u32;
    let size = DescriptorPoolSize { descriptor_type: 1, count };
    let dpi = DescriptorPoolCreateInfo { s_type: ST_DESCRIPTOR_POOL, next: std::ptr::null(), flags: 0, max_sets: count, pool_size_count: 1, pool_sizes: &size };
    check("vkCreateDescriptorPool", (c.fns.create_descriptor_pool)(d, &dpi, std::ptr::null(), &mut c.pool))?;
    let layouts = vec![c.set_layout; real.len()];
    let dai = DescriptorSetAllocateInfo { s_type: ST_DESCRIPTOR_SET_ALLOCATE, next: std::ptr::null(), pool: c.pool, count, layouts: layouts.as_ptr() };
    let mut sets = vec![0u64; real.len()];
    check("vkAllocateDescriptorSets", (c.fns.allocate_descriptor_sets)(d, &dai, sets.as_mut_ptr()))?;
    let infos: Vec<DescriptorImageInfo> = c
        .proxy_views
        .iter()
        .map(|&v| DescriptorImageInfo { sampler: c.sampler, image_view: v, layout: LAYOUT_SHADER_READ_ONLY })
        .collect();
    let writes: Vec<WriteDescriptorSet> = sets
        .iter()
        .zip(&infos)
        .map(|(&set, info)| WriteDescriptorSet {
            s_type: ST_WRITE_DESCRIPTOR_SET,
            next: std::ptr::null(),
            set,
            binding: 0,
            array_element: 0,
            count: 1,
            descriptor_type: 1,
            image_info: info,
            buffer_info: std::ptr::null(),
            texel_buffer_view: std::ptr::null(),
        })
        .collect();
    (c.fns.update_descriptor_sets)(d, writes.len() as u32, writes.as_ptr(), 0, std::ptr::null());

    // The render pass: the real image is written whole, so its previous
    // contents are not loaded, and it leaves in the layout a present needs.
    let att = AttachmentDescription {
        flags: 0,
        format: req.format,
        samples: 1,
        load_op: 2,
        store_op: 0,
        stencil_load_op: 2,
        stencil_store_op: 1,
        initial_layout: LAYOUT_UNDEFINED,
        final_layout: LAYOUT_PRESENT_SRC,
    };
    let aref = AttachmentReference { attachment: 0, layout: LAYOUT_COLOR_ATTACHMENT };
    let sub = SubpassDescription {
        flags: 0,
        pipeline_bind_point: 0,
        input_count: 0,
        inputs: std::ptr::null(),
        color_count: 1,
        colors: &aref,
        resolves: std::ptr::null(),
        depth: std::ptr::null(),
        preserve_count: 0,
        preserves: std::ptr::null(),
    };
    let dep = SubpassDependency {
        src_subpass: SUBPASS_EXTERNAL,
        dst_subpass: 0,
        src_stage: STAGE_COLOR_ATTACHMENT_OUTPUT,
        dst_stage: STAGE_COLOR_ATTACHMENT_OUTPUT,
        src_access: 0,
        dst_access: ACCESS_COLOR_ATTACHMENT_WRITE,
        dependency_flags: 0,
    };
    let rpi = RenderPassCreateInfo {
        s_type: ST_RENDER_PASS,
        next: std::ptr::null(),
        flags: 0,
        attachment_count: 1,
        attachments: &att,
        subpass_count: 1,
        subpasses: &sub,
        dependency_count: 1,
        dependencies: &dep,
    };
    check("vkCreateRenderPass", (c.fns.create_render_pass)(d, &rpi, std::ptr::null(), &mut c.render_pass))?;

    let (rw, rh) = c.real_extent;
    for &v in &c.real_views {
        let fi = FramebufferCreateInfo {
            s_type: ST_FRAMEBUFFER,
            next: std::ptr::null(),
            flags: 0,
            render_pass: c.render_pass,
            attachment_count: 1,
            attachments: &v,
            width: rw,
            height: rh,
            layers: 1,
        };
        let mut fb = 0u64;
        check("vkCreateFramebuffer", (c.fns.create_framebuffer)(d, &fi, std::ptr::null(), &mut fb))?;
        c.framebuffers.push(fb);
    }

    let name = c"main".as_ptr();
    let stages = [
        PipelineShaderStage { s_type: ST_PIPELINE_SHADER_STAGE, next: std::ptr::null(), flags: 0, stage: 1, module: c.shaders[0], name, specialization: std::ptr::null() },
        PipelineShaderStage { s_type: ST_PIPELINE_SHADER_STAGE, next: std::ptr::null(), flags: 0, stage: 0x10, module: c.shaders[1], name, specialization: std::ptr::null() },
    ];
    let vi = PipelineVertexInput { s_type: ST_PIPELINE_VERTEX_INPUT, next: std::ptr::null(), flags: 0, binding_count: 0, bindings: std::ptr::null(), attribute_count: 0, attributes: std::ptr::null() };
    let ia = PipelineInputAssembly { s_type: ST_PIPELINE_INPUT_ASSEMBLY, next: std::ptr::null(), flags: 0, topology: 3, primitive_restart: 0 };
    let vp = Viewport { x: 0.0, y: 0.0, width: rw as f32, height: rh as f32, min_depth: 0.0, max_depth: 1.0 };
    let sc = Rect2D { x: 0, y: 0, width: rw, height: rh };
    let vs = PipelineViewport { s_type: ST_PIPELINE_VIEWPORT, next: std::ptr::null(), flags: 0, viewport_count: 1, viewports: &vp, scissor_count: 1, scissors: &sc };
    let rs = PipelineRasterization {
        s_type: ST_PIPELINE_RASTERIZATION,
        next: std::ptr::null(),
        flags: 0,
        depth_clamp: 0,
        rasterizer_discard: 0,
        polygon_mode: 0,
        cull_mode: 0,
        front_face: 0,
        depth_bias: 0,
        depth_bias_constant: 0.0,
        depth_bias_clamp: 0.0,
        depth_bias_slope: 0.0,
        line_width: 1.0,
    };
    let ms = PipelineMultisample { s_type: ST_PIPELINE_MULTISAMPLE, next: std::ptr::null(), flags: 0, samples: 1, sample_shading: 0, min_sample_shading: 0.0, sample_mask: std::ptr::null(), alpha_to_coverage: 0, alpha_to_one: 0 };
    let cba = PipelineColorBlendAttachment { blend_enable: 0, src_color: 0, dst_color: 0, color_op: 0, src_alpha: 0, dst_alpha: 0, alpha_op: 0, write_mask: 0xF };
    let cb = PipelineColorBlend { s_type: ST_PIPELINE_COLOR_BLEND, next: std::ptr::null(), flags: 0, logic_op_enable: 0, logic_op: 0, attachment_count: 1, attachments: &cba, blend_constants: [0.0; 4] };
    let gpi = GraphicsPipelineCreateInfo {
        s_type: ST_GRAPHICS_PIPELINE,
        next: std::ptr::null(),
        flags: 0,
        stage_count: 2,
        stages: stages.as_ptr(),
        vertex_input: &vi,
        input_assembly: &ia,
        tessellation: std::ptr::null(),
        viewport: &vs,
        rasterization: &rs,
        multisample: &ms,
        depth_stencil: std::ptr::null(),
        color_blend: &cb,
        dynamic: std::ptr::null(),
        layout: c.pipeline_layout,
        render_pass: c.render_pass,
        subpass: 0,
        base_pipeline: 0,
        base_pipeline_index: -1,
    };
    check("vkCreateGraphicsPipelines", (c.fns.create_graphics_pipelines)(d, 0, 1, &gpi, std::ptr::null(), &mut c.pipeline))?;

    // The pool, buffers, semaphores and the timestamp pool.
    let cpi = CommandPoolCreateInfo { s_type: ST_COMMAND_POOL, next: std::ptr::null(), flags: 0, queue_family_index: queue_family };
    check("vkCreateCommandPool", (c.fns.create_command_pool)(d, &cpi, std::ptr::null(), &mut c.cmd_pool))?;
    let cai = CommandBufferAllocateInfo { s_type: ST_COMMAND_BUFFER_ALLOCATE, next: std::ptr::null(), pool: c.cmd_pool, level: 0, count };
    c.cmds = vec![0u64; real.len()];
    check("vkAllocateCommandBuffers", (c.fns.allocate_command_buffers)(d, &cai, c.cmds.as_mut_ptr()))?;
    for _ in 0..real.len() {
        let sci = SemaphoreCreateInfo { s_type: ST_SEMAPHORE_CREATE_INFO, next: std::ptr::null(), flags: 0 };
        let mut s = 0u64;
        check("vkCreateSemaphore", (c.fns.create_semaphore)(d, &sci, std::ptr::null(), &mut s))?;
        c.semaphores.push(s);
    }
    c.used = vec![false; real.len()];
    let timed = c.ts_mask != 0 && c.ts_period_ns > 0.0;
    if timed {
        let qi = QueryPoolCreateInfo { s_type: ST_QUERY_POOL_CREATE_INFO, next: std::ptr::null(), flags: 0, query_type: 2, query_count: 2 * count, pipeline_statistics: 0 };
        check("vkCreateQueryPool", (c.fns.create_query_pool)(d, &qi, std::ptr::null(), &mut c.query_pool))?;
    }

    let mode = filter_mode();
    let (sw, sh) = req.extent;
    let pc: [f32; 8] = [1.0 / sw as f32, 1.0 / sh as f32, sw as f32, sh as f32, f32::from_bits(mode), 0.0, 0.0, 0.0];
    for i in 0..real.len() {
        record(c, i, real[i], sets[i], &pc)?;
    }
    println!(
        "[android] render-scale: swapchain {:#x}: engine {}x{} format {} usage {:#x} -> window {}x{}, {} images, filter {}, timestamps {}",
        swapchain,
        sw,
        sh,
        req.format,
        req.usage,
        rw,
        rh,
        real.len(),
        if mode == 1 { "GSR1" } else { "bilinear" },
        if timed { "on" } else { "unavailable" },
    );
    Ok(())
}

fn record(c: &Chain, i: usize, real: u64, set: u64, pc: &[f32; 8]) -> Result<(), String> {
    let cb = c.cmds[i];
    let f = &c.fns;
    let bi = CommandBufferBeginInfo { s_type: ST_COMMAND_BUFFER_BEGIN, next: std::ptr::null(), flags: 0, inheritance: std::ptr::null() };
    check("vkBeginCommandBuffer", (f.begin_command_buffer)(cb, &bi))?;
    let timed = c.query_pool != 0;
    if timed {
        (f.cmd_reset_query_pool)(cb, c.query_pool, 2 * i as u32, 2);
        (f.cmd_write_timestamp)(cb, STAGE_TOP_OF_PIPE, c.query_pool, 2 * i as u32);
    }
    let barrier = |image: u64, src_access, dst_access, old, new| ImageMemoryBarrier {
        s_type: ST_IMAGE_MEMORY_BARRIER,
        next: std::ptr::null(),
        src_access,
        dst_access,
        old_layout: old,
        new_layout: new,
        src_family: u32::MAX,
        dst_family: u32::MAX,
        image,
        range: COLOR_RANGE,
    };
    // The engine's image, as the engine left it, to something a shader reads.
    let b = barrier(c.proxies[i], ACCESS_MEMORY_WRITE, ACCESS_SHADER_READ, LAYOUT_PRESENT_SRC, LAYOUT_SHADER_READ_ONLY);
    (f.cmd_pipeline_barrier)(cb, STAGE_ALL_COMMANDS, STAGE_FRAGMENT_SHADER, 0, 0, std::ptr::null(), 0, std::ptr::null(), 1, &b);
    let rp = RenderPassBeginInfo {
        s_type: ST_RENDER_PASS_BEGIN,
        next: std::ptr::null(),
        render_pass: c.render_pass,
        framebuffer: c.framebuffers[i],
        render_area: Rect2D { x: 0, y: 0, width: c.real_extent.0, height: c.real_extent.1 },
        clear_count: 0,
        clears: std::ptr::null(),
    };
    (f.cmd_begin_render_pass)(cb, &rp, 0);
    (f.cmd_bind_pipeline)(cb, 0, c.pipeline);
    (f.cmd_bind_descriptor_sets)(cb, 0, c.pipeline_layout, 0, 1, &set, 0, std::ptr::null());
    (f.cmd_push_constants)(cb, c.pipeline_layout, 0x10, 0, 32, pc.as_ptr() as *const c_void);
    (f.cmd_draw)(cb, 3, 1, 0, 0);
    (f.cmd_end_render_pass)(cb);
    // Hand the engine's image back in the layout it believes it is in.
    let b = barrier(c.proxies[i], ACCESS_SHADER_READ, ACCESS_MEMORY_READ, LAYOUT_SHADER_READ_ONLY, LAYOUT_PRESENT_SRC);
    (f.cmd_pipeline_barrier)(cb, STAGE_FRAGMENT_SHADER, STAGE_BOTTOM_OF_PIPE, 0, 0, std::ptr::null(), 0, std::ptr::null(), 1, &b);
    if timed {
        (f.cmd_write_timestamp)(cb, STAGE_BOTTOM_OF_PIPE, c.query_pool, 2 * i as u32 + 1);
    }
    let _ = real;
    check("vkEndCommandBuffer", (f.end_command_buffer)(cb))
}

/// `timestampPeriod` (nanoseconds per tick) and `timestampValidBits` for a queue family.
fn timestamp_facts(physical_device: usize, family: u32) -> (f64, u32) {
    let props = super::vulkan::host_instance_proc(c"vkGetPhysicalDeviceProperties");
    let qprops = super::vulkan::host_instance_proc(c"vkGetPhysicalDeviceQueueFamilyProperties");
    if props.is_null() || qprops.is_null() {
        return (0.0, 0);
    }
    // SAFETY: the host's functions for exactly these names.
    let (props, qprops): (extern "C" fn(usize, *mut u8), extern "C" fn(usize, *mut u32, *mut QueueFamilyProperties)) =
        unsafe { (std::mem::transmute(props), std::mem::transmute(qprops)) };
    // `VkPhysicalDeviceProperties` is 824 bytes; `limits` starts at 296 and
    // `timestampPeriod` is 424 bytes into it (checked against the C headers).
    let mut buf = vec![0u8; 1024];
    props(physical_device, buf.as_mut_ptr());
    let period = f32::from_ne_bytes(buf[296 + 424..296 + 428].try_into().unwrap()) as f64;
    let mut n = 0u32;
    qprops(physical_device, &mut n, std::ptr::null_mut());
    let mut q: Vec<QueueFamilyProperties> = (0..n).map(|_| QueueFamilyProperties { queue_flags: 0, queue_count: 0, timestamp_valid_bits: 0, granularity: [0; 3] }).collect();
    qprops(physical_device, &mut n, q.as_mut_ptr());
    (period, q.get(family as usize).map_or(0, |p| p.timestamp_valid_bits))
}

fn teardown(c: &Chain) {
    let d = c.device;
    let f = &c.fns;
    (f.device_wait_idle)(d);
    let n = std::ptr::null();
    for &x in &c.semaphores { (f.destroy_semaphore)(d, x, n); }
    if c.query_pool != 0 { (f.destroy_query_pool)(d, c.query_pool, n); }
    if c.cmd_pool != 0 { (f.destroy_command_pool)(d, c.cmd_pool, n); }
    if c.pipeline != 0 { (f.destroy_pipeline)(d, c.pipeline, n); }
    for &x in &c.framebuffers { (f.destroy_framebuffer)(d, x, n); }
    if c.render_pass != 0 { (f.destroy_render_pass)(d, c.render_pass, n); }
    if c.pool != 0 { (f.destroy_descriptor_pool)(d, c.pool, n); }
    if c.pipeline_layout != 0 { (f.destroy_pipeline_layout)(d, c.pipeline_layout, n); }
    if c.set_layout != 0 { (f.destroy_descriptor_set_layout)(d, c.set_layout, n); }
    for &x in &c.shaders { if x != 0 { (f.destroy_shader_module)(d, x, n); } }
    if c.sampler != 0 { (f.destroy_sampler)(d, c.sampler, n); }
    for &x in c.proxy_views.iter().chain(&c.real_views) { (f.destroy_image_view)(d, x, n); }
    for &x in &c.proxies { (f.destroy_image)(d, x, n); }
    for &x in &c.memories { (f.free_memory)(d, x, n); }
}

/// `vkDestroySwapchainKHR`: drop this swapchain's proxies and pass, if it had any.
pub fn forget(swapchain: u64) {
    let gone = CHAINS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|m| m.remove(&swapchain));
    if let Some(c) = gone {
        teardown(&c);
        println!("[android] render-scale: swapchain {swapchain:#x} destroyed after {} upscaled frames", c.frames);
    }
}

/// `vkGetSwapchainImagesKHR` for a swapchain with proxies: theirs, not the real ones.
pub fn images(swapchain: u64, count: *mut u32, out: *mut u64) -> Option<i32> {
    let g = CHAINS.lock().unwrap_or_else(|e| e.into_inner());
    let c = g.as_ref()?.get(&swapchain)?;
    // SAFETY: the engine's own out-parameters, as the specification defines them.
    unsafe {
        if out.is_null() {
            *count = c.proxies.len() as u32;
            return Some(VK_SUCCESS);
        }
        let n = (*count as usize).min(c.proxies.len());
        std::ptr::copy_nonoverlapping(c.proxies.as_ptr(), out, n);
        let rc = if n < c.proxies.len() { VK_INCOMPLETE } else { VK_SUCCESS };
        *count = n as u32;
        Some(rc)
    }
}

/// A copy of the engine's present info, waiting on the pass instead of on
/// whatever the engine waited on. Lives on the caller's stack for the call.
pub struct PresentPatch {
    info: [u8; 64],
    wait: [u64; 1],
}

impl Default for PresentPatch {
    fn default() -> Self {
        Self { info: [0; 64], wait: [0] }
    }
}

impl PresentPatch {
    pub fn as_ptr(&self) -> *const c_void {
        self.info.as_ptr() as *const c_void
    }
}

/// Before the engine's present goes to the driver: run the upscale for the
/// swapchain it names, if that swapchain has a pass. Returns whether `patch`
/// is to be presented in place of the engine's info.
///
/// Only a present of exactly one swapchain with no `pNext` the pass would be
/// wrong to drop is handled; anything else is forwarded as it came, which
/// shows the engine's small image in the corner and is said once.
pub fn before_present(queue: u64, info: *const c_void, patch: &mut PresentPatch) -> bool {
    if info.is_null() {
        return false;
    }
    // SAFETY: a valid `VkPresentInfoKHR`; offsets are the C layout (checked in tests).
    let (wait_count, waits, count, chains, indices) = unsafe {
        let b = info as *const u8;
        (
            std::ptr::read_unaligned(b.add(16) as *const u32),
            std::ptr::read_unaligned(b.add(24) as *const *const u64),
            std::ptr::read_unaligned(b.add(32) as *const u32),
            std::ptr::read_unaligned(b.add(40) as *const *const u64),
            std::ptr::read_unaligned(b.add(48) as *const *const u32),
        )
    };
    if count != 1 || chains.is_null() || indices.is_null() {
        return false;
    }
    // SAFETY: `count` is 1, so both arrays have an element.
    let (swapchain, index) = unsafe { (std::ptr::read_unaligned(chains), std::ptr::read_unaligned(indices) as usize) };
    let mut g = CHAINS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(c) = g.as_mut().and_then(|m| m.get_mut(&swapchain)) else { return false };
    if index >= c.cmds.len() {
        return false;
    }

    // The previous use of this image's timestamps, now finished: the present
    // that released it completed before the engine could acquire it again.
    if c.query_pool != 0 && c.used[index] {
        let mut t = [0u64; 2];
        let rc = (c.fns.get_query_pool_results)(c.device, c.query_pool, 2 * index as u32, 2, 16, t.as_mut_ptr() as *mut c_void, 8, 1);
        if rc == VK_SUCCESS {
            let ticks = t[1].wrapping_sub(t[0]) & c.ts_mask;
            c.gpu_ns += ticks as f64 * c.ts_period_ns;
            c.gpu_frames += 1;
            if c.gpu_frames == 300 {
                println!(
                    "[android] render-scale: upscale pass GPU time {:.3} ms (mean of {} frames, timestamps top to bottom of the pass)",
                    c.gpu_ns / c.gpu_frames as f64 / 1.0e6,
                    c.gpu_frames
                );
                c.gpu_ns = 0.0;
                c.gpu_frames = 0;
            }
        }
    }

    let stages: Vec<u32> = vec![STAGE_ALL_COMMANDS; wait_count as usize];
    let sig = c.semaphores[index];
    let cb = c.cmds[index];
    let si = SubmitInfo {
        s_type: ST_SUBMIT_INFO,
        next: std::ptr::null(),
        wait_count,
        waits,
        wait_stages: stages.as_ptr(),
        command_buffer_count: 1,
        command_buffers: &cb,
        signal_count: 1,
        signals: &sig,
    };
    let rc = (c.fns.queue_submit)(queue, 1, &si, 0);
    if rc != VK_SUCCESS {
        static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !SAID.swap(true, Ordering::Relaxed) {
            println!("[android] render-scale: vkQueueSubmit for the upscale returned {rc}; presenting the engine's image as it is");
        }
        return false;
    }
    c.used[index] = true;
    c.frames += 1;
    UPSCALED.fetch_add(1, Ordering::Relaxed);

    // SAFETY: copying the 64 bytes of a `VkPresentInfoKHR`; the two fields
    // that name what to wait on are then pointed at the pass's semaphore.
    unsafe {
        std::ptr::copy_nonoverlapping(info as *const u8, patch.info.as_mut_ptr(), 64);
        patch.wait[0] = sig;
        std::ptr::write_unaligned(patch.info.as_mut_ptr().add(16) as *mut u32, 1);
        std::ptr::write_unaligned(patch.info.as_mut_ptr().add(24) as *mut *const u64, patch.wait.as_ptr());
    }
    true
}

#[allow(dead_code)]
const _: i32 = VK_ERROR_INITIALIZATION_FAILED;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    /// Sizes from `sizeof` in C against `vulkan/vulkan.h` 1.4 on x86-64.
    #[test]
    fn structures_match_the_c_layout() {
        assert_eq!(size_of::<ImageCreateInfo>(), 88);
        assert_eq!(size_of::<MemoryRequirements>(), 24);
        assert_eq!(size_of::<MemoryAllocateInfo>(), 32);
        assert_eq!(size_of::<ImageViewCreateInfo>(), 80);
        assert_eq!(size_of::<SamplerCreateInfo>(), 80);
        assert_eq!(size_of::<ShaderModuleCreateInfo>(), 40);
        assert_eq!(size_of::<DescriptorSetLayoutBinding>(), 24);
        assert_eq!(size_of::<DescriptorSetLayoutCreateInfo>(), 32);
        assert_eq!(size_of::<PushConstantRange>(), 12);
        assert_eq!(size_of::<PipelineLayoutCreateInfo>(), 48);
        assert_eq!(size_of::<DescriptorPoolSize>(), 8);
        assert_eq!(size_of::<DescriptorPoolCreateInfo>(), 40);
        assert_eq!(size_of::<DescriptorSetAllocateInfo>(), 40);
        assert_eq!(size_of::<DescriptorImageInfo>(), 24);
        assert_eq!(size_of::<WriteDescriptorSet>(), 64);
        assert_eq!(size_of::<AttachmentDescription>(), 36);
        assert_eq!(size_of::<AttachmentReference>(), 8);
        assert_eq!(size_of::<SubpassDescription>(), 72);
        assert_eq!(size_of::<SubpassDependency>(), 28);
        assert_eq!(size_of::<RenderPassCreateInfo>(), 64);
        assert_eq!(size_of::<FramebufferCreateInfo>(), 64);
        assert_eq!(size_of::<PipelineShaderStage>(), 48);
        assert_eq!(size_of::<PipelineVertexInput>(), 48);
        assert_eq!(size_of::<PipelineInputAssembly>(), 32);
        assert_eq!(size_of::<Viewport>(), 24);
        assert_eq!(size_of::<Rect2D>(), 16);
        assert_eq!(size_of::<PipelineViewport>(), 48);
        assert_eq!(size_of::<PipelineRasterization>(), 64);
        assert_eq!(size_of::<PipelineMultisample>(), 48);
        assert_eq!(size_of::<PipelineColorBlendAttachment>(), 32);
        assert_eq!(size_of::<PipelineColorBlend>(), 56);
        assert_eq!(size_of::<GraphicsPipelineCreateInfo>(), 144);
        assert_eq!(size_of::<CommandPoolCreateInfo>(), 24);
        assert_eq!(size_of::<CommandBufferAllocateInfo>(), 32);
        assert_eq!(size_of::<CommandBufferBeginInfo>(), 32);
        assert_eq!(size_of::<RenderPassBeginInfo>(), 64);
        assert_eq!(size_of::<ImageMemoryBarrier>(), 72);
        assert_eq!(size_of::<SemaphoreCreateInfo>(), 24);
        assert_eq!(size_of::<SubmitInfo>(), 72);
        assert_eq!(size_of::<QueryPoolCreateInfo>(), 32);
        assert_eq!(size_of::<QueueFamilyProperties>(), 24);
    }

    #[test]
    fn scale_parses_only_what_it_should() {
        assert_eq!(parse_scale("0.67"), Some(0.67));
        assert_eq!(parse_scale(" 0.5 "), Some(0.5));
        assert_eq!(parse_scale("1"), None);
        assert_eq!(parse_scale("1.0"), None);
        assert_eq!(parse_scale("0.1"), None);
        assert_eq!(parse_scale("nan"), None);
        assert_eq!(parse_scale("two thirds"), None);
    }

    #[test]
    fn extents_round_trip_within_a_pixel() {
        for s in [0.5f32, 0.67, 0.75, 0.9] {
            for real in [(1280u32, 754u32), (1920, 1080), (3440, 1440), (1, 1)] {
                let e = scaled(real, s);
                assert!(e.0 >= 1 && e.1 >= 1);
                note_real_extent(7, real);
                assert_eq!(real_extent_for(7, e, s), real);
                let back = real_extent_for(8, e, s);
                assert!(back.0.abs_diff(real.0) <= 2 && back.1.abs_diff(real.1) <= 2 || real.0 < 4, "{real:?} {s}");
            }
        }
    }
}
