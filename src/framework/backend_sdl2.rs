use core::mem;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use imgui::{DrawCmd, DrawData, ImString, TextureId, Ui};
use imgui::internal::RawWrapper;
use sdl2::{EventPump, keyboard, pixels, Sdl};
use sdl2::event::{Event, WindowEvent};
use sdl2::gfx::primitives::DrawRenderer;
use sdl2::keyboard::Scancode;
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{Texture, TextureCreator, WindowCanvas};
use sdl2::surface::Surface;
use sdl2::video::WindowContext;

use crate::common::Color;
use crate::framework::backend::{Backend, BackendEventLoop, BackendRenderer, BackendTexture, SpriteBatchCommand};
use crate::framework::context::Context;
use crate::framework::error::{GameError, GameResult};
use crate::framework::graphics::{BlendMode, imgui_context};
use crate::framework::keyboard::ScanCode;
use crate::framework::ui::init_imgui;
use crate::Game;

pub struct SDL2Backend {
    context: Sdl,
}

impl SDL2Backend {
    pub fn new() -> GameResult<Box<dyn Backend>> {
        let context = sdl2::init().map_err(|e| GameError::WindowError(e))?;

        let backend = SDL2Backend {
            context,
        };

        Ok(Box::new(backend))
    }
}

impl Backend for SDL2Backend {
    fn create_event_loop(&self) -> GameResult<Box<dyn BackendEventLoop>> {
        SDL2EventLoop::new(&self.context)
    }
}

struct SDL2EventLoop {
    event_pump: EventPump,
    refs: Rc<RefCell<SDL2Context>>,
}

struct SDL2Context {
    canvas: WindowCanvas,
    texture_creator: TextureCreator<WindowContext>,
    blend_mode: sdl2::render::BlendMode,
}

impl SDL2EventLoop {
    pub fn new(sdl: &Sdl) -> GameResult<Box<dyn BackendEventLoop>> {
        sdl2::hint::set("SDL_HINT_RENDER_DRIVER", "opengles2");

        let event_pump = sdl.event_pump().map_err(|e| GameError::WindowError(e))?;
        let video = sdl.video().map_err(|e| GameError::WindowError(e))?;
        let window = video.window("Cave Story (doukutsu-rs)", 640, 480)
            .position_centered()
            .resizable()
            .build()
            .map_err(|e| GameError::WindowError(e.to_string()))?;

        let canvas = window.into_canvas()
            .accelerated()
            .present_vsync()
            .build()
            .map_err(|e| GameError::RenderError(e.to_string()))?;

        let texture_creator = canvas.texture_creator();

        let event_loop = SDL2EventLoop {
            event_pump,
            refs: Rc::new(RefCell::new(SDL2Context {
                canvas,
                texture_creator,
                blend_mode: sdl2::render::BlendMode::Blend,
            })),
        };

        Ok(Box::new(event_loop))
    }
}

impl BackendEventLoop for SDL2EventLoop {
    fn run(&mut self, game: &mut Game, ctx: &mut Context) {
        let state = unsafe { &mut *game.state.get() };

        let (imgui, imgui_sdl2) = unsafe {
            let renderer: &Box<SDL2Renderer> = std::mem::transmute(ctx.renderer.as_ref().unwrap());

            (&mut *renderer.imgui.as_ptr(), &mut *renderer.imgui_event.as_ptr())
        };

        {
            let (width, height) = self.refs.borrow().canvas.window().size();
            ctx.screen_size = (width.max(1) as f32, height.max(1) as f32);

            imgui.io_mut().display_size = [ctx.screen_size.0, ctx.screen_size.1];
            let _ = state.handle_resize(ctx);
        }

        loop {
            for event in self.event_pump.poll_iter() {
                imgui_sdl2.handle_event(imgui, &event);

                match event {
                    Event::Quit { .. } => {
                        state.shutdown();
                    }
                    Event::Window { win_event, .. } => {
                        match win_event {
                            WindowEvent::Shown => {}
                            WindowEvent::Hidden => {}
                            WindowEvent::SizeChanged(width, height) => {
                                ctx.screen_size = (width.max(1) as f32, height.max(1) as f32);

                                if let Some(renderer) = ctx.renderer.as_ref() {
                                    if let Ok(imgui) = renderer.imgui() {
                                        imgui.io_mut().display_size = [ctx.screen_size.0, ctx.screen_size.1];
                                    }
                                }
                                state.handle_resize(ctx);
                            }
                            _ => {}
                        }
                    }
                    Event::KeyDown { scancode, repeat, .. } => {
                        if let Some(scancode) = scancode {
                            if let Some(drs_scan) = conv_scancode(scancode) {
                                game.key_down_event(drs_scan, repeat);
                                ctx.keyboard_context.set_key(drs_scan, true);
                            }
                        }
                    }
                    Event::KeyUp { scancode, .. } => {
                        if let Some(scancode) = scancode {
                            if let Some(drs_scan) = conv_scancode(scancode) {
                                ctx.keyboard_context.set_key(drs_scan, false);
                            }
                        }
                    }
                    _ => {}
                }
            }

            game.update(ctx).unwrap();

            if state.shutdown {
                log::info!("Shutting down...");
                break;
            }

            if state.next_scene.is_some() {
                mem::swap(&mut game.scene, &mut state.next_scene);
                state.next_scene = None;

                game.scene.as_mut().unwrap().init(state, ctx).unwrap();
                game.loops = 0;
                state.frame_time = 0.0;
            }

            imgui_sdl2.prepare_frame(imgui.io_mut(), self.refs.borrow().canvas.window(), &self.event_pump.mouse_state());
            game.draw(ctx).unwrap();
        }
    }

    fn new_renderer(&self) -> GameResult<Box<dyn BackendRenderer>> {
        SDL2Renderer::new(self.refs.clone())
    }
}

struct SDL2Renderer {
    refs: Rc<RefCell<SDL2Context>>,
    imgui: Rc<RefCell<imgui::Context>>,
    imgui_event: Rc<RefCell<imgui_sdl2::ImguiSdl2>>,
    imgui_textures: HashMap<TextureId, SDL2Texture>,
}

impl SDL2Renderer {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(refs: Rc<RefCell<SDL2Context>>) -> GameResult<Box<dyn BackendRenderer>> {
        let mut imgui = init_imgui()?;
        let mut imgui_textures = HashMap::new();

        imgui.set_renderer_name(ImString::new("SDL2Renderer"));
        {
            let mut refs = refs.clone();
            let mut fonts = imgui.fonts();
            let id = fonts.tex_id;
            let font_tex = fonts.build_rgba32_texture();

            let mut texture = refs.borrow_mut().texture_creator
                .create_texture_streaming(PixelFormatEnum::RGBA32, font_tex.width, font_tex.height)
                .map_err(|e| GameError::RenderError(e.to_string()))?;

            texture.set_blend_mode(sdl2::render::BlendMode::Blend);
            texture.with_lock(None, |buffer: &mut [u8], pitch: usize| {
                for y in 0..(font_tex.height as usize) {
                    for x in 0..(font_tex.width as usize) {
                        let offset = y * pitch + x * 4;
                        let data_offset = (y * font_tex.width as usize + x) * 4;

                        buffer[offset] = font_tex.data[data_offset];
                        buffer[offset + 1] = font_tex.data[data_offset + 1];
                        buffer[offset + 2] = font_tex.data[data_offset + 2];
                        buffer[offset + 3] = font_tex.data[data_offset + 3];
                    }
                }
            }).map_err(|e| GameError::RenderError(e.to_string()))?;

            imgui_textures.insert(id, SDL2Texture {
                refs: refs.clone(),
                texture: Some(texture),
                width: font_tex.width as u16,
                height: font_tex.height as u16,
                commands: vec![],
            });
        }

        let imgui_sdl2 = unsafe {
            let refs = &mut *refs.as_ptr();
            imgui_sdl2::ImguiSdl2::new(&mut imgui, refs.canvas.window())
        };

        Ok(Box::new(SDL2Renderer {
            refs,
            imgui: Rc::new(RefCell::new(imgui)),
            imgui_event: Rc::new(RefCell::new(imgui_sdl2)),
            imgui_textures,
        }))
    }
}

fn to_sdl(color: Color) -> pixels::Color {
    let (r, g, b, a) = color.to_rgba();
    pixels::Color::RGBA(r, g, b, a)
}

unsafe fn set_raw_target(renderer: *mut sdl2::sys::SDL_Renderer, raw_texture: *mut sdl2::sys::SDL_Texture) -> GameResult {
    if sdl2::sys::SDL_SetRenderTarget(renderer, raw_texture) == 0 {
        Ok(())
    } else {
        Err(GameError::RenderError(sdl2::get_error()))
    }
}

fn min3(x: f32, y: f32, z: f32) -> f32 {
    if x < y && x < z { x } else if y < z { y } else { z }
}

fn max3(x: f32, y: f32, z: f32) -> f32 {
    if x > y && x > z { x } else if y > z { y } else { z }
}

impl BackendRenderer for SDL2Renderer {
    fn clear(&mut self, color: Color) {
        let mut refs = self.refs.borrow_mut();

        refs.canvas.set_draw_color(to_sdl(color));
        refs.canvas.clear();
    }

    fn present(&mut self) -> GameResult {
        let mut refs = self.refs.borrow_mut();

        refs.canvas.present();

        Ok(())
    }

    fn create_texture_mutable(&mut self, width: u16, height: u16) -> GameResult<Box<dyn BackendTexture>> {
        let mut refs = self.refs.borrow_mut();

        let mut texture = refs.texture_creator
            .create_texture_target(PixelFormatEnum::RGBA32, width as u32, height as u32)
            .map_err(|e| GameError::RenderError(e.to_string()))?;

        Ok(Box::new(SDL2Texture {
            refs: self.refs.clone(),
            texture: Some(texture),
            width,
            height,
            commands: vec![],
        }))
    }

    fn create_texture(&mut self, width: u16, height: u16, data: &[u8]) -> GameResult<Box<dyn BackendTexture>> {
        let mut refs = self.refs.borrow_mut();

        let mut texture = refs.texture_creator
            .create_texture_streaming(PixelFormatEnum::RGBA32, width as u32, height as u32)
            .map_err(|e| GameError::RenderError(e.to_string()))?;

        texture.set_blend_mode(sdl2::render::BlendMode::Blend);
        texture.with_lock(None, |buffer: &mut [u8], pitch: usize| {
            for y in 0..(height as usize) {
                for x in 0..(width as usize) {
                    let offset = y * pitch + x * 4;
                    let data_offset = (y * width as usize + x) * 4;

                    buffer[offset] = data[data_offset];
                    buffer[offset + 1] = data[data_offset + 1];
                    buffer[offset + 2] = data[data_offset + 2];
                    buffer[offset + 3] = data[data_offset + 3];
                }
            }
        }).map_err(|e| GameError::RenderError(e.to_string()))?;

        Ok(Box::new(SDL2Texture {
            refs: self.refs.clone(),
            texture: Some(texture),
            width,
            height,
            commands: vec![],
        }))
    }

    fn set_blend_mode(&mut self, blend: BlendMode) -> GameResult {
        let mut refs = self.refs.borrow_mut();

        refs.blend_mode = match blend {
            BlendMode::Add => sdl2::render::BlendMode::Add,
            BlendMode::Alpha => sdl2::render::BlendMode::Blend,
            BlendMode::Multiply => sdl2::render::BlendMode::Mod,
        };

        Ok(())
    }

    fn set_render_target(&mut self, texture: Option<&Box<dyn BackendTexture>>) -> GameResult {
        let renderer = self.refs.borrow().canvas.raw();

        // todo: horribly unsafe
        match texture {
            Some(texture) => unsafe {
                let sdl2_texture: &Box<SDL2Texture> = std::mem::transmute(texture);

                if let Some(target) = sdl2_texture.texture.as_ref() {
                    set_raw_target(renderer, target.raw());
                } else {
                    set_raw_target(renderer, std::ptr::null_mut());
                }
            }
            None => unsafe {
                set_raw_target(renderer, std::ptr::null_mut());
            }
        }

        Ok(())
    }

    fn imgui(&self) -> GameResult<&mut imgui::Context> {
        unsafe {
            Ok(&mut *self.imgui.as_ptr())
        }
    }

    fn render_imgui(&mut self, draw_data: &DrawData) -> GameResult {
        let mut refs = self.refs.borrow_mut();

        for draw_list in draw_data.draw_lists() {
            for cmd in draw_list.commands() {
                match cmd {
                    DrawCmd::Elements { count, cmd_params } => {
                        refs.canvas.set_clip_rect(Some(sdl2::rect::Rect::new(
                            cmd_params.clip_rect[0] as i32,
                            cmd_params.clip_rect[1] as i32,
                            (cmd_params.clip_rect[2] - cmd_params.clip_rect[0]) as u32,
                            (cmd_params.clip_rect[3] - cmd_params.clip_rect[1]) as u32,
                        )));

                        let idx_buffer = draw_list.idx_buffer();
                        let mut vert_x = [0i16; 6];
                        let mut vert_y = [0i16; 6];
                        let mut min = [0f32; 2];
                        let mut max = [0f32; 2];
                        let mut tex_pos = [0f32; 4];
                        let mut is_rect = false;

                        for i in (0..count).step_by(3) {
                            if is_rect {
                                is_rect = false;
                                continue;
                            }

                            let v1 = draw_list.vtx_buffer()[cmd_params.vtx_offset + idx_buffer[cmd_params.idx_offset + i] as usize];
                            let v2 = draw_list.vtx_buffer()[cmd_params.vtx_offset + idx_buffer[cmd_params.idx_offset + i + 1] as usize];
                            let v3 = draw_list.vtx_buffer()[cmd_params.vtx_offset + idx_buffer[cmd_params.idx_offset + i + 2] as usize];

                            vert_x[0] = (v1.pos[0] - 0.5) as i16;
                            vert_y[0] = (v1.pos[1] - 0.5) as i16;
                            vert_x[1] = (v2.pos[0] - 0.5) as i16;
                            vert_y[1] = (v2.pos[1] - 0.5) as i16;
                            vert_x[2] = (v3.pos[0] - 0.5) as i16;
                            vert_y[2] = (v3.pos[1] - 0.5) as i16;

                            #[allow(clippy::float_cmp)]
                            if i < count - 3 {
                                let v4 = draw_list.vtx_buffer()[cmd_params.vtx_offset + idx_buffer[cmd_params.idx_offset + i + 3] as usize];
                                let v5 = draw_list.vtx_buffer()[cmd_params.vtx_offset + idx_buffer[cmd_params.idx_offset + i + 4] as usize];
                                let v6 = draw_list.vtx_buffer()[cmd_params.vtx_offset + idx_buffer[cmd_params.idx_offset + i + 5] as usize];

                                min[0] = min3(v1.pos[0], v2.pos[0], v3.pos[0]);
                                min[1] = min3(v1.pos[1], v2.pos[1], v3.pos[1]);
                                max[0] = max3(v1.pos[0], v2.pos[0], v3.pos[0]);
                                max[1] = max3(v1.pos[1], v2.pos[1], v3.pos[1]);

                                is_rect = (v1.pos[0] == min[0] || v1.pos[0] == max[0]) &&
                                    (v1.pos[1] == min[1] || v1.pos[1] == max[1]) &&
                                    (v2.pos[0] == min[0] || v2.pos[0] == max[0]) &&
                                    (v2.pos[1] == min[1] || v2.pos[1] == max[1]) &&
                                    (v3.pos[0] == min[0] || v3.pos[0] == max[0]) &&
                                    (v3.pos[1] == min[1] || v3.pos[1] == max[1]) &&
                                    (v4.pos[0] == min[0] || v4.pos[0] == max[0]) &&
                                    (v4.pos[1] == min[1] || v4.pos[1] == max[1]) &&
                                    (v5.pos[0] == min[0] || v5.pos[0] == max[0]) &&
                                    (v5.pos[1] == min[1] || v5.pos[1] == max[1]) &&
                                    (v6.pos[0] == min[0] || v6.pos[0] == max[0]) &&
                                    (v6.pos[1] == min[1] || v6.pos[1] == max[1]);

                                if is_rect {
                                    tex_pos[0] = min3(v1.uv[0], v2.uv[0], v3.uv[0]);
                                    tex_pos[1] = min3(v1.uv[1], v2.uv[1], v3.uv[1]);
                                    tex_pos[2] = max3(v1.uv[0], v2.uv[0], v3.uv[0]);
                                    tex_pos[3] = max3(v1.uv[1], v2.uv[1], v3.uv[1]);
                                }
                            }

                            if let Some(surf) = self.imgui_textures.get_mut(&cmd_params.texture_id) {
                                unsafe {
                                    if is_rect {
                                        let src = sdl2::rect::Rect::new((tex_pos[0] * surf.width as f32) as i32,
                                                                        (tex_pos[1] * surf.height as f32) as i32,
                                                                        ((tex_pos[2] - tex_pos[0]) * surf.width as f32) as u32,
                                                                        ((tex_pos[3] - tex_pos[1]) * surf.height as f32) as u32);
                                        let dest = sdl2::rect::Rect::new(min[0] as i32,
                                                                         min[1] as i32,
                                                                         (max[0] - min[0]) as u32,
                                                                         (max[1] - min[1]) as u32);

                                        let tex = surf.texture.as_mut().unwrap();
                                        tex.set_color_mod(v1.col[0], v1.col[1], v1.col[2]);
                                        tex.set_alpha_mod(v1.col[3]);

                                        refs.canvas.copy(tex, src, dest);
                                    } else {
                                        sdl2::sys::gfx::primitives::filledPolygonRGBA(
                                            refs.canvas.raw(),
                                            vert_x.as_ptr(),
                                            vert_y.as_ptr(),
                                            3,
                                            v1.col[0],
                                            v1.col[1],
                                            v1.col[2],
                                            v1.col[3],
                                        );
                                    }
                                }
                            }
                        }

                        refs.canvas.set_clip_rect(None);
                    }
                    DrawCmd::ResetRenderState => {}
                    DrawCmd::RawCallback { callback, raw_cmd } => unsafe {
                        callback(draw_list.raw(), raw_cmd)
                    }
                }
            }
        }

        Ok(())
    }

    fn prepare_frame<'ui>(&self, ui: &Ui<'ui>) -> GameResult {
        Ok(())
    }
}

impl SDL2Renderer {}

struct SDL2Texture {
    refs: Rc<RefCell<SDL2Context>>,
    texture: Option<Texture>,
    width: u16,
    height: u16,
    commands: Vec<SpriteBatchCommand>,
}

impl BackendTexture for SDL2Texture {
    fn dimensions(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    fn add(&mut self, command: SpriteBatchCommand) {
        self.commands.push(command);
    }

    fn clear(&mut self) {
        self.commands.clear();
    }

    fn draw(&mut self) -> GameResult {
        match self.texture.as_mut() {
            None => Ok(()),
            Some(texture) => {
                let mut refs = self.refs.borrow_mut();
                for command in self.commands.iter() {
                    match command {
                        SpriteBatchCommand::DrawRect(src, dest) => {
                            texture.set_color_mod(255, 255, 255);
                            texture.set_alpha_mod(255);
                            texture.set_blend_mode(refs.blend_mode);

                            refs.canvas.copy(texture,
                                             Some(sdl2::rect::Rect::new(src.left.round() as i32, src.top.round() as i32, src.width().round() as u32, src.height().round() as u32)),
                                             Some(sdl2::rect::Rect::new(dest.left.round() as i32, dest.top.round() as i32, dest.width().round() as u32, dest.height().round() as u32)))
                                .map_err(|e| GameError::RenderError(e.to_string()))?;
                        }
                        SpriteBatchCommand::DrawRectTinted(src, dest, color) => {
                            let (r, g, b, a) = color.to_rgba();
                            texture.set_color_mod(r, g, b);
                            texture.set_alpha_mod(a);
                            texture.set_blend_mode(refs.blend_mode);

                            refs.canvas.copy(texture,
                                             Some(sdl2::rect::Rect::new(src.left.round() as i32, src.top.round() as i32, src.width().round() as u32, src.height().round() as u32)),
                                             Some(sdl2::rect::Rect::new(dest.left.round() as i32, dest.top.round() as i32, dest.width().round() as u32, dest.height().round() as u32)))
                                .map_err(|e| GameError::RenderError(e.to_string()))?;
                        }
                    }
                }

                Ok(())
            }
        }
    }
}

impl Drop for SDL2Texture {
    fn drop(&mut self) {
        let mut texture_opt = None;
        mem::swap(&mut self.texture, &mut texture_opt);

        if let Some(texture) = texture_opt {
            unsafe { texture.destroy(); }
        }
    }
}

fn conv_scancode(code: keyboard::Scancode) -> Option<ScanCode> {
    match code {
        Scancode::A => Some(ScanCode::A),
        Scancode::B => Some(ScanCode::B),
        Scancode::C => Some(ScanCode::C),
        Scancode::D => Some(ScanCode::D),
        Scancode::E => Some(ScanCode::E),
        Scancode::F => Some(ScanCode::F),
        Scancode::G => Some(ScanCode::G),
        Scancode::H => Some(ScanCode::H),
        Scancode::I => Some(ScanCode::I),
        Scancode::J => Some(ScanCode::J),
        Scancode::K => Some(ScanCode::K),
        Scancode::L => Some(ScanCode::L),
        Scancode::M => Some(ScanCode::M),
        Scancode::N => Some(ScanCode::N),
        Scancode::O => Some(ScanCode::O),
        Scancode::P => Some(ScanCode::P),
        Scancode::Q => Some(ScanCode::Q),
        Scancode::R => Some(ScanCode::R),
        Scancode::S => Some(ScanCode::S),
        Scancode::T => Some(ScanCode::T),
        Scancode::U => Some(ScanCode::U),
        Scancode::V => Some(ScanCode::V),
        Scancode::W => Some(ScanCode::W),
        Scancode::X => Some(ScanCode::X),
        Scancode::Y => Some(ScanCode::Y),
        Scancode::Z => Some(ScanCode::Z),
        Scancode::Num1 => Some(ScanCode::Key1),
        Scancode::Num2 => Some(ScanCode::Key2),
        Scancode::Num3 => Some(ScanCode::Key3),
        Scancode::Num4 => Some(ScanCode::Key4),
        Scancode::Num5 => Some(ScanCode::Key5),
        Scancode::Num6 => Some(ScanCode::Key6),
        Scancode::Num7 => Some(ScanCode::Key7),
        Scancode::Num8 => Some(ScanCode::Key8),
        Scancode::Num9 => Some(ScanCode::Key9),
        Scancode::Num0 => Some(ScanCode::Key0),
        Scancode::Return => Some(ScanCode::Return),
        Scancode::Escape => Some(ScanCode::Escape),
        Scancode::Backspace => Some(ScanCode::Backspace),
        Scancode::Tab => Some(ScanCode::Tab),
        Scancode::Space => Some(ScanCode::Space),
        Scancode::Minus => Some(ScanCode::Minus),
        Scancode::Equals => Some(ScanCode::Equals),
        Scancode::LeftBracket => Some(ScanCode::LBracket),
        Scancode::RightBracket => Some(ScanCode::RBracket),
        Scancode::Backslash => Some(ScanCode::Backslash),
        Scancode::NonUsHash => Some(ScanCode::NonUsHash),
        Scancode::Semicolon => Some(ScanCode::Semicolon),
        Scancode::Apostrophe => Some(ScanCode::Apostrophe),
        Scancode::Grave => Some(ScanCode::Grave),
        Scancode::Comma => Some(ScanCode::Comma),
        Scancode::Period => Some(ScanCode::Period),
        Scancode::Slash => Some(ScanCode::Slash),
        Scancode::CapsLock => Some(ScanCode::Capslock),
        Scancode::F1 => Some(ScanCode::F1),
        Scancode::F2 => Some(ScanCode::F2),
        Scancode::F3 => Some(ScanCode::F3),
        Scancode::F4 => Some(ScanCode::F4),
        Scancode::F5 => Some(ScanCode::F5),
        Scancode::F6 => Some(ScanCode::F6),
        Scancode::F7 => Some(ScanCode::F7),
        Scancode::F8 => Some(ScanCode::F8),
        Scancode::F9 => Some(ScanCode::F9),
        Scancode::F10 => Some(ScanCode::F10),
        Scancode::F11 => Some(ScanCode::F11),
        Scancode::F12 => Some(ScanCode::F12),
        Scancode::PrintScreen => Some(ScanCode::Sysrq),
        Scancode::ScrollLock => Some(ScanCode::Scrolllock),
        Scancode::Pause => Some(ScanCode::Pause),
        Scancode::Insert => Some(ScanCode::Insert),
        Scancode::Home => Some(ScanCode::Home),
        Scancode::PageUp => Some(ScanCode::PageUp),
        Scancode::Delete => Some(ScanCode::Delete),
        Scancode::End => Some(ScanCode::End),
        Scancode::PageDown => Some(ScanCode::PageDown),
        Scancode::Right => Some(ScanCode::Right),
        Scancode::Left => Some(ScanCode::Left),
        Scancode::Down => Some(ScanCode::Down),
        Scancode::Up => Some(ScanCode::Up),
        Scancode::NumLockClear => Some(ScanCode::Numlock),
        Scancode::KpDivide => Some(ScanCode::NumpadDivide),
        Scancode::KpMultiply => Some(ScanCode::NumpadMultiply),
        Scancode::KpMinus => Some(ScanCode::NumpadSubtract),
        Scancode::KpPlus => Some(ScanCode::NumpadAdd),
        Scancode::KpEnter => Some(ScanCode::NumpadEnter),
        Scancode::Kp1 => Some(ScanCode::Numpad1),
        Scancode::Kp2 => Some(ScanCode::Numpad2),
        Scancode::Kp3 => Some(ScanCode::Numpad3),
        Scancode::Kp4 => Some(ScanCode::Numpad4),
        Scancode::Kp5 => Some(ScanCode::Numpad5),
        Scancode::Kp6 => Some(ScanCode::Numpad6),
        Scancode::Kp7 => Some(ScanCode::Numpad7),
        Scancode::Kp8 => Some(ScanCode::Numpad8),
        Scancode::Kp9 => Some(ScanCode::Numpad9),
        Scancode::Kp0 => Some(ScanCode::Numpad0),
        Scancode::NonUsBackslash => Some(ScanCode::NonUsBackslash),
        Scancode::Application => Some(ScanCode::Apps),
        Scancode::Power => Some(ScanCode::Power),
        Scancode::KpEquals => Some(ScanCode::NumpadEquals),
        Scancode::F13 => Some(ScanCode::F13),
        Scancode::F14 => Some(ScanCode::F14),
        Scancode::F15 => Some(ScanCode::F15),
        Scancode::F16 => Some(ScanCode::F16),
        Scancode::F17 => Some(ScanCode::F17),
        Scancode::F18 => Some(ScanCode::F18),
        Scancode::F19 => Some(ScanCode::F19),
        Scancode::F20 => Some(ScanCode::F20),
        Scancode::F21 => Some(ScanCode::F21),
        Scancode::F22 => Some(ScanCode::F22),
        Scancode::F23 => Some(ScanCode::F23),
        Scancode::F24 => Some(ScanCode::F24),
        Scancode::Stop => Some(ScanCode::Stop),
        Scancode::Cut => Some(ScanCode::Cut),
        Scancode::Copy => Some(ScanCode::Copy),
        Scancode::Paste => Some(ScanCode::Paste),
        Scancode::Mute => Some(ScanCode::Mute),
        Scancode::VolumeUp => Some(ScanCode::VolumeUp),
        Scancode::VolumeDown => Some(ScanCode::VolumeDown),
        Scancode::KpComma => Some(ScanCode::NumpadComma),
        Scancode::SysReq => Some(ScanCode::Sysrq),
        Scancode::Return2 => Some(ScanCode::NumpadEnter),
        Scancode::LCtrl => Some(ScanCode::LControl),
        Scancode::LShift => Some(ScanCode::LShift),
        Scancode::LAlt => Some(ScanCode::LAlt),
        Scancode::LGui => Some(ScanCode::LWin),
        Scancode::RCtrl => Some(ScanCode::RControl),
        Scancode::RShift => Some(ScanCode::RShift),
        Scancode::RAlt => Some(ScanCode::RAlt),
        Scancode::RGui => Some(ScanCode::RWin),
        Scancode::AudioNext => Some(ScanCode::NextTrack),
        Scancode::AudioPrev => Some(ScanCode::PrevTrack),
        Scancode::AudioStop => Some(ScanCode::MediaStop),
        Scancode::AudioPlay => Some(ScanCode::PlayPause),
        Scancode::AudioMute => Some(ScanCode::Mute),
        Scancode::MediaSelect => Some(ScanCode::MediaSelect),
        Scancode::Mail => Some(ScanCode::Mail),
        Scancode::Calculator => Some(ScanCode::Calculator),
        Scancode::Sleep => Some(ScanCode::Sleep),
        _ => None,
    }
}
