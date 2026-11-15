//! Корпус острова на видеокарте: стекло считается для каждой точки отдельной программой (шейдером).
//!
//! Форма задаётся как набор скруглённых «капель», которые плавно сливаются друг с другом, если оказываются
//! рядом, — так от пилюли отпочковываются капли таймера и звонка. Внутри формы фон экрана преломляется
//! у кромки, цвета чуть расслаиваются, по ободку идёт свет со стороны курсора; снаружи — тень и свечение.

use eframe::glow::{self, HasContext};

pub const MAX_BLOBS: usize = 4;

#[derive(Clone, Copy)]
pub struct Params {
    /// капли: центр x, центр y, полуширина, полувысота (в точках окна) и радиус скругления
    pub blobs: [[f32; 4]; MAX_BLOBS],
    pub radius: [f32; MAX_BLOBS],
    pub count: i32,
    /// насколько мягко капли сливаются (в точках)
    pub merge: f32,
    pub time: f32,
    pub ripple: f32,
    /// положение курсора в точках окна — оттуда «светит»
    pub light: [f32; 2],
    /// область снимка фона в точках окна (лево, верх, право, низ)
    pub cap: [f32; 4],
    pub has_tex: bool,
    pub tint: [f32; 3],
    /// доля затемнения поверх фона, 0…1
    pub dim: f32,
    /// цвет и сила свечения
    pub glow: [f32; 4],
    /// ширина скруглённой кромки в точках
    pub thick: f32,
    /// ход световой полосы 0…1 (вне этого промежутка её нет)
    pub sheen: f32,
}

pub struct Fx {
    program: glow::Program,
    vao: glow::VertexArray,
}

const VS: &str = r#"#version 330
const vec2 v[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
void main() { gl_Position = vec4(v[gl_VertexID], 0.0, 1.0); }
"#;

const FS: &str = r#"#version 330
uniform vec2 u_org;        // левый край и верх области в пикселях кадра (верх — от нижнего края)
uniform float u_ppp;
uniform vec4 u_blob[4];
uniform float u_rad[4];
uniform int u_n;
uniform float u_k;
uniform float u_time;
uniform float u_rip;
uniform vec2 u_light;
uniform sampler2D u_tex;
uniform vec4 u_cap;
uniform float u_has;
uniform vec3 u_tint;
uniform float u_dim;
uniform vec4 u_glow;
uniform float u_thick;
uniform float u_sheen;
out vec4 o;

float sdBox(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

// плавное объединение: рядом стоящие капли срастаются «шейкой»
float smin(float a, float b, float k) {
    float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

float map(vec2 p) {
    float d = 1e5;
    for (int i = 0; i < 4; i++) {
        if (i >= u_n) break;
        float di = sdBox(p - u_blob[i].xy, u_blob[i].zw, u_rad[i]);
        d = (i == 0) ? di : smin(d, di, u_k);
    }
    // рябь по кромке
    d += u_rip * 0.5 * sin(p.x * 0.11 + p.y * 0.07 - u_time * 7.5);
    return d;
}

float hash(vec2 p) { return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453); }

void main() {
    vec2 px = vec2(gl_FragCoord.x - u_org.x, u_org.y - gl_FragCoord.y);
    vec2 p = px / u_ppp;
    float d = map(p);
    float body = clamp(0.5 - d * u_ppp, 0.0, 1.0);

    // тень: широкая мягкая под островом и узкая плотная у самого края — остров «лежит» над экраном
    float shadow = 0.32 * smoothstep(36.0, 0.0, map(p - vec2(0.0, 10.0))) + 0.20 * smoothstep(7.0, 0.0, map(p - vec2(0.0, 2.5)));
    float halo = u_glow.a * exp(-max(d, 0.0) / 11.0);
    vec3 outside = u_glow.rgb * halo * 0.85;
    float outside_a = clamp(shadow + halo * 0.55, 0.0, 1.0);
    if (body <= 0.0) { o = vec4(outside, outside_a); return; }

    // толстое литое стекло: кромка скруглена по дуге, у каждой точки своя «высота» и наклон
    vec2 e = vec2(0.75, 0.0);
    vec2 n = normalize(vec2(map(p + e.xy) - map(p - e.xy), map(p + e.yx) - map(p - e.yx)) + 1e-5);
    float t = clamp(1.0 + d / u_thick, 0.0, 1.0);   // 0 на плоской середине, 1 у самой кромки
    float z = sqrt(max(1.0 - t * t, 0.0));
    vec3 N = normalize(vec3(n * t, z + 0.08));

    // свет неподвижный: сверху, чуть слева
    vec2 l2 = normalize(vec2(-0.30, -0.95));
    vec3 L = normalize(vec3(l2 * 0.9, 0.42));
    vec3 H = normalize(L + vec3(0.0, 0.0, 1.0));
    float ndh = max(dot(N, H), 0.0);
    float away = max(dot(n, -l2), 0.0);
    float toward = max(dot(n, l2), 0.0);

    vec3 col;
    if (u_has > 0.5) {
        // линза: у кромки картинка под стеклом стягивается к краю, цвета расходятся
        vec2 size = u_cap.zw - u_cap.xy;
        float bend = t * t * t;
        vec2 uv = (p - n * bend * 28.0 - u_cap.xy) / size;
        vec2 split = n * bend * 4.5 / size;
        col = vec3(texture(u_tex, uv + split).r, texture(u_tex, uv).g, texture(u_tex, uv - split).b);
        float g = dot(col, vec3(0.3, 0.59, 0.11));
        col = mix(vec3(g), col, 1.35);
        col = mix(col, u_tint, u_dim);
    } else {
        // без снимка фона: плотное тёмное стекло с мягким перепадом тона
        float k = clamp((p.y - (u_blob[0].y - u_blob[0].w)) / (u_blob[0].w * 2.0), 0.0, 1.0);
        col = mix(u_tint * 2.9 + 0.045, u_tint * 1.15 + 0.012, k);
    }

    // толща: к кромке стекло темнее, с теневой стороны внутри лежит тень…
    col *= 0.80 + 0.20 * z;
    col *= 1.0 - 0.34 * away * smoothstep(0.15, 0.7, t) * (1.0 - smoothstep(0.7, 1.0, t));
    // …а у самого края с той же стороны свет выходит наружу яркой полоской
    col += (vec3(0.55) + 0.45 * u_glow.rgb * u_glow.a) * 0.20 * pow(away, 1.2) * smoothstep(0.74, 1.0, t);
    // блик на скруглении со стороны света и тонкая светлая линия по ободку
    col += vec3(1.0) * 0.05 * pow(ndh, 9.0);
    col += vec3(1.0) * smoothstep(2.0, 0.0, -d) * (0.07 + 0.24 * toward * toward);
    col += vec3(1.0) * 0.08 * pow(1.0 - N.z, 2.5);
    col += u_glow.rgb * u_glow.a * pow(t, 3.0) * 0.5;

    // световая полоса, пробегающая по корпусу при раскрытии и наведении
    if (u_sheen > 0.0 && u_sheen < 1.0) {
        float x0 = u_blob[0].x - u_blob[0].z - 60.0 + (2.0 * u_blob[0].z + 120.0) * u_sheen;
        float sx = p.x + (p.y - u_blob[0].y) * 0.55;
        float band = exp(-pow((sx - x0) / 24.0, 2.0));
        col += vec3(1.0) * band * 0.13 * sin(3.14159 * u_sheen);
    }

    col += (hash(px) - 0.5) * 0.014;
    col = clamp(col, 0.0, 1.0);

    o = vec4(col * body + outside * (1.0 - body), body + outside_a * (1.0 - body));
}
"#;

fn log(text: &str) {
    let dir = crate::store::dir();
    let _ = std::fs::create_dir_all(&dir);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("errors.log")) {
        let _ = writeln!(f, "шейдер: {text}");
    }
}

impl Fx {
    pub fn new(gl: &glow::Context) -> Option<Self> {
        unsafe {
            let program = gl.create_program().ok()?;
            for (kind, src) in [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, FS)] {
                let shader = gl.create_shader(kind).ok()?;
                gl.shader_source(shader, src);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    log(&gl.get_shader_info_log(shader));
                    return None;
                }
                gl.attach_shader(program, shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                log(&gl.get_program_info_log(program));
                return None;
            }
            let vao = gl.create_vertex_array().ok()?;
            Some(Self { program, vao })
        }
    }

    /// left и top_from_bottom — положение области рисования в пикселях кадра.
    pub fn paint(&self, gl: &glow::Context, tex: Option<glow::Texture>, left: f32, top_from_bottom: f32, ppp: f32, p: &Params) {
        unsafe {
            gl.use_program(Some(self.program));
            let loc = |name: &str| gl.get_uniform_location(self.program, name);
            gl.uniform_2_f32(loc("u_org").as_ref(), left, top_from_bottom);
            gl.uniform_1_f32(loc("u_ppp").as_ref(), ppp);
            let flat: Vec<f32> = p.blobs.iter().flatten().copied().collect();
            gl.uniform_4_f32_slice(loc("u_blob").as_ref(), &flat);
            gl.uniform_1_f32_slice(loc("u_rad").as_ref(), &p.radius);
            gl.uniform_1_i32(loc("u_n").as_ref(), p.count);
            gl.uniform_1_f32(loc("u_k").as_ref(), p.merge);
            gl.uniform_1_f32(loc("u_time").as_ref(), p.time);
            gl.uniform_1_f32(loc("u_rip").as_ref(), p.ripple);
            gl.uniform_2_f32(loc("u_light").as_ref(), p.light[0], p.light[1]);
            gl.uniform_4_f32(loc("u_cap").as_ref(), p.cap[0], p.cap[1], p.cap[2], p.cap[3]);
            gl.uniform_1_f32(loc("u_has").as_ref(), if p.has_tex && tex.is_some() { 1.0 } else { 0.0 });
            gl.uniform_3_f32(loc("u_tint").as_ref(), p.tint[0], p.tint[1], p.tint[2]);
            gl.uniform_1_f32(loc("u_dim").as_ref(), p.dim);
            gl.uniform_4_f32(loc("u_glow").as_ref(), p.glow[0], p.glow[1], p.glow[2], p.glow[3]);
            gl.uniform_1_f32(loc("u_thick").as_ref(), p.thick);
            gl.uniform_1_f32(loc("u_sheen").as_ref(), p.sheen);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, tex);
            gl.uniform_1_i32(loc("u_tex").as_ref(), 0);
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
    }
}
