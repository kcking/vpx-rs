use common::VPXCodec;
use ffi::vpx::*;

use std::mem::{uninitialized, zeroed};
use std::mem;
use std::ptr;
use std::sync::Arc;

use data::frame::{Frame, VideoInfo};
use data::frame::{PictureType, new_default_frame};
use data::pixel::formats::YUV420;

use self::vpx_codec_err_t::*;

fn frame_from_img(img: vpx_image_t) -> Frame {
    use self::vpx_img_fmt_t::*;
    let f = match img.fmt {
        VPX_IMG_FMT_I420 => YUV420,
        _ => panic!("TODO: support more pixel formats"),
    };
    let v = VideoInfo {
        pic_type: PictureType::UNKNOWN,
        width: img.d_w as usize,
        height: img.d_h as usize,
        format: Arc::new(*f),
    };

    let mut f = new_default_frame(v, None);

    let src = img.planes.iter().map(|v| *v as *const u8);
    let linesize = img.stride.iter().map(|l| *l as usize);

    f.copy_from_raw_parts(src, linesize);
    f
}

use std::marker::PhantomData;

pub struct VP9Decoder<T> {
    pub(crate) ctx: vpx_codec_ctx,
    pub(crate) iter: vpx_codec_iter_t,
    private_data: PhantomData<T>
}

impl<T> VP9Decoder<T> {
    pub fn new() -> Result<VP9Decoder<T>, vpx_codec_err_t> {
        let mut dec = VP9Decoder {
            ctx: unsafe { uninitialized() },
            iter: ptr::null(),
            private_data: PhantomData,
        };
        let cfg = unsafe { zeroed() };

        let ret = unsafe {
            vpx_codec_dec_init_ver(
                &mut dec.ctx as *mut vpx_codec_ctx,
                vpx_codec_vp9_dx(),
                &cfg as *const vpx_codec_dec_cfg_t,
                0,
                VPX_DECODER_ABI_VERSION as i32,
            )
        };
        match ret {
            VPX_CODEC_OK => Ok(dec),
            _ => Err(ret),
        }
    }

    pub fn decode<O>(&mut self, data: &[u8], private: O) -> Result<(), vpx_codec_err_t>
        where O: Into<Option<T>> {
        let priv_data = private
            .into()
            .map(|v| {
                Box::into_raw(Box::new(v))
            })
            .unwrap_or(ptr::null_mut());
        let ret = unsafe {
            vpx_codec_decode(
                &mut self.ctx,
                data.as_ptr(),
                data.len() as u32,
                mem::transmute(priv_data),
                0,
            )
        };

        // Safety measure to not call get_frame on an invalid iterator
        self.iter = ptr::null();

        match ret {
            VPX_CODEC_OK => {
                mem::forget(priv_data);
                Ok(())
            },
            _ => Err(ret),
        }
    }

    pub fn flush(&mut self) -> Result<(), vpx_codec_err_t> {
        let ret = unsafe {
             vpx_codec_decode(
                &mut self.ctx,
                ptr::null(),
                0,
                ptr::null_mut(),
                0,
            )
        };

        self.iter = ptr::null();

        match ret {
            VPX_CODEC_OK => {
                Ok(())
            },
            _ => Err(ret),
        }
    }

    pub fn get_frame(&mut self) -> Option<(Frame, Option<Box<T>>)> {
        let img = unsafe { vpx_codec_get_frame(&mut self.ctx, &mut self.iter) };
        mem::forget(img);

        if img.is_null() {
            None
        } else {
            let im = unsafe { *img };
            let priv_data = if im.user_priv.is_null() {
                None
            } else {
                let p : *mut T = unsafe { mem::transmute(im.user_priv) };
                Some(unsafe { Box::from_raw(p) })
            };
            let frame = frame_from_img(im);
            Some((frame, priv_data))
        }
    }
}

impl<T> Drop for VP9Decoder<T> {
    fn drop(&mut self) {
        unsafe { vpx_codec_destroy(&mut self.ctx) };
    }
}

impl<T> VPXCodec for VP9Decoder<T> {
    fn get_context<'a>(&'a mut self) -> &'a mut vpx_codec_ctx {
        &mut self.ctx
    }
}

#[cfg(feature="codec-trait")]
mod decoder_trait {
    use super::*;
    use codec::decoder::*;
    use codec::error::*;
    use data::packet::Packet;
    use data::frame::ArcFrame;
    use data::timeinfo::TimeInfo;
    use std::sync::Arc;

    struct Des {
        descr: Descr,
    }

    impl Descriptor for Des {
        fn create(&self) -> Box<Decoder> {
            Box::new(VP9Decoder::new().unwrap())
        }

        fn describe<'a>(&'a self) -> &'a Descr {
            &self.descr
        }
    }

    impl Decoder for VP9Decoder<TimeInfo> {
        fn set_extradata(&mut self, _extra: &[u8]) {
            // No-op
        }
        fn send_packet(&mut self, pkt: &Packet) -> Result<()> {
            self.decode(&pkt.data, pkt.t).map_err(|_err| unimplemented!())
        }
        fn receive_frame(&mut self) -> Result<ArcFrame> {
            self.get_frame()
                .map(|(mut f, t)| {
                    f.t = t.map(|b| *b).unwrap();
                    Arc::new(f)
                })
                .ok_or(ErrorKind::MoreDataNeeded.into())
        }
        fn flush(&mut self) -> Result<()> {
            self.flush().map_err(|_err| unimplemented!())
        }
        fn configure(&mut self) -> Result<()> {
            Ok(())
        }
    }

    pub const VP9_DESCR: &Descriptor = &Des {
        descr: Descr {
            codec: "vp9",
            name: "vpx",
            desc: "libvpx VP9 decoder",
            mime: "video/VP9",
        },
    };
}

#[cfg(feature="codec-trait")]
pub use self::decoder_trait::VP9_DESCR;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn init() {
        let mut d = VP9Decoder::<()>::new().unwrap();

        println!("{}", d.error_to_str());
    }

    use super::super::encoder::tests as enc;
    use super::super::encoder::VPXPacket;
    use data::timeinfo::TimeInfo;
    use data::rational::*;
    #[test]
    fn decode() {
        let w = 800;
        let h = 600;

        let t = TimeInfo {
            pts: Some(0),
            dts: Some(0),
            duration: Some(1),
            timebase: Some(Rational64::new(1, 1000)),
        };

        let mut e = enc::setup(w, h, &t);
        let mut f = enc::setup_frame(w, h, &t);

        let mut d = VP9Decoder::<()>::new().unwrap();
        let mut out = 0;

        for i in 0..100 {
            e.encode(&f).unwrap();
            f.t.pts = Some(i);

            println!("{:#?}", f);
            loop {
                let p = e.get_packet();

                if p.is_none() {
                    break;
                } else {
                    if let VPXPacket::Packet(ref pkt) = p.unwrap() {
                        let _ = d.decode(&pkt.data, None).unwrap();

                        // No multiframe expected.
                        if let Some(f) = d.get_frame() {
                            out = 1;
                            println!("{:#?}", f);
                        }
                    }
                }
            }
        }

        if out != 1 {
            panic!("No frame decoded");
        }
    }

    #[cfg(all(test, feature = "codec-trait"))]
    #[test]
    fn decode_codec_trait() {
        use codec::common::CodecList;
        use codec::encoder as en;
        use codec::decoder as de;
        use codec::error::*;
        use super::super::encoder::VP9_DESCR as ENC;
        use super::super::decoder::VP9_DESCR as DEC;
        use std::sync::Arc;

        let encoders = en::Codecs::from_list(&[ENC]);
        let decoders = de::Codecs::from_list(&[DEC]);
        let mut enc = en::Context::by_name(&encoders, "vp9").unwrap();
        let mut dec = de::Context::by_name(&decoders, "vp9").unwrap();
        let w = 200;
        let h = 200;

        enc.set_option("w", w as u64).unwrap();
        enc.set_option("h", h as u64).unwrap();
        enc.set_option("timebase", (1, 1000)).unwrap();

        let t = TimeInfo {
            pts: Some(0),
            dts: Some(0),
            duration: Some(1),
            timebase: Some(Rational64::new(1, 1000)),
        };

        enc.configure().unwrap();
        dec.configure().unwrap();

        let mut f = Arc::new(enc::setup_frame(w, h, &t));
        let mut enc_out = 0;
        let mut dec_out = 0;
        for i in 0..100 {
            Arc::get_mut(&mut f).unwrap().t.pts = Some(i);

            println!("Sending {}", i);
            enc.send_frame(&f).unwrap();

            loop {
                match enc.receive_packet() {
                    Ok(p) => {
                        println!("{:#?}", p);
                        enc_out = 1;
                        dec.send_packet(&p).unwrap();

                        loop {
                            match dec.receive_frame() {
                                Ok(f) => {
                                    println!("{:#?}", f);
                                    dec_out = 1;
                                },
                                Err(e) => match e.kind() {
                                    &ErrorKind::MoreDataNeeded => break,
                                    _ => unimplemented!()
                                }
                            }
                        }
                    },
                    Err(e) => match e.kind() {
                        &ErrorKind::MoreDataNeeded => break,
                        _ => unimplemented!()
                    }
                }
            }
        }

        enc.flush().unwrap();

        loop {
            match enc.receive_packet() {
                Ok(p) => {
                    println!("{:#?}", p);
                    enc_out = 1
                },
                Err(e) => match e.kind() {
                    &ErrorKind::MoreDataNeeded => break,
                    _ => unimplemented!()
                }
            }
        }

        if enc_out != 1 || dec_out != 1 {
            panic!();
        }
    }

}
