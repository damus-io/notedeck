use std::path::{Path, PathBuf};

use egui::TextureHandle;

use crate::media::budget::VariantTexCache;
use crate::TextureState;
use crate::{
    jobs::{
        CompleteResponse, JobOutput, JobPackage, JobRun, MediaJobKind, MediaJobResult,
        MediaJobSender, NoOutputRun, RunType,
    },
    ImageType,
};
use crate::{
    media::{
        encrypted::{decrypt, EncryptedImeta, EncryptedMedia},
        images::{
            buffer_to_color_image, normalize_image_type_for_request, parse_img_response,
            process_image, should_persist_full_content, TextureRequestKey,
        },
        load_texture_checked,
    },
    network::http_req,
    MediaCache,
};

pub struct StaticImgTexCache {
    pub(crate) textures: VariantTexCache<TextureHandle>,
    static_img_cache_path: PathBuf,
}

impl StaticImgTexCache {
    pub fn new(static_img_cache_path: PathBuf) -> Self {
        Self {
            textures: Default::default(),
            static_img_cache_path,
        }
    }

    /// Returns true when any size variant for the URL has been requested.
    pub fn contains(&self, url: &str) -> bool {
        self.textures.contains(url)
    }

    /// Records the host pass this cache is now serving; see
    /// [`VariantTexCache::begin_pass`].
    pub fn begin_pass(&mut self, pass_nr: u64) {
        self.textures.begin_pass(pass_nr);
    }

    pub(crate) fn set_pending(&mut self, request_key: TextureRequestKey, pass_nr: u64) {
        self.set_state(request_key, TextureState::Pending, pass_nr);
    }

    pub(crate) fn set_state(
        &mut self,
        request_key: TextureRequestKey,
        state: TextureState<TextureHandle>,
        pass_nr: u64,
    ) {
        let TextureRequestKey { url, variant } = request_key;
        self.textures.set_state(url, variant, state, pass_nr);
    }

    pub fn request(
        &self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        imgtype: ImageType,
        encrypted: &EncryptedMedia,
    ) {
        let _ = self.get_or_request(jobs, ctx, url, imgtype, encrypted);
    }

    /// The texture for `url`, or a fetch job for it when there is none yet.
    ///
    /// When `url` is registered in `encrypted`, the job is handed its key
    /// and decrypts what it downloads. The lookup happens only when a job
    /// is created, not on every frame.
    pub fn get_or_request(
        &self,
        jobs: &MediaJobSender,
        ctx: &egui::Context,
        url: &str,
        imgtype: ImageType,
        encrypted: &EncryptedMedia,
    ) -> &TextureState<TextureHandle> {
        let imgtype = normalize_image_type_for_request(imgtype);
        let request_variant = TextureRequestKey::variant_for_image_type(imgtype);
        if let Some(res) = self.textures.get(url, request_variant) {
            return res;
        }
        let request_key = TextureRequestKey::from_variant(url, request_variant);
        let request_id = request_key.to_job_id();

        let key = MediaCache::key(url);
        let path = self.static_img_cache_path.join(key);

        if path.exists() {
            let ctx = ctx.clone();
            let url = url.to_owned();
            let request_key = request_key.clone();
            if let Err(e) = jobs.send(JobPackage::new(
                request_id.clone(),
                MediaJobKind::StaticImg {
                    request_key: request_key.clone(),
                },
                RunType::Output(JobRun::Sync(Box::new(move || {
                    JobOutput::Complete(CompleteResponse::new(MediaJobResult::StaticImg(
                        fetch_static_img_from_disk(ctx.clone(), &url, &request_key, imgtype, &path),
                    )))
                }))),
            )) {
                tracing::error!("{e}");
            }
        } else {
            let secret = encrypted.get(url).cloned();
            let url = url.to_owned();
            let ctx = ctx.clone();
            let request_key = request_key.clone();
            if let Err(e) = jobs.send(JobPackage::new(
                request_id.clone(),
                MediaJobKind::StaticImg {
                    request_key: request_key.clone(),
                },
                RunType::Output(JobRun::Async(Box::pin(fetch_static_img_from_net(
                    url,
                    request_key,
                    ctx,
                    self.static_img_cache_path.clone(),
                    imgtype,
                    secret,
                )))),
            )) {
                tracing::error!("{e}");
            }
        }

        &TextureState::Pending
    }
}

/// The image a request shows and the image it writes to the disk cache, which
/// keeps full resolution when the request wants a smaller display copy.
struct DisplayAndDiskImages {
    display: egui::ColorImage,
    disk: egui::ColorImage,
}

/// Builds display output and disk output for a static image request.
fn parse_display_and_disk_images(
    content_type: Option<&str>,
    bytes: &[u8],
    imgtype: ImageType,
) -> Result<DisplayAndDiskImages, crate::Error> {
    let display_img = parse_img_response(content_type, bytes, imgtype)?;

    let disk_img = if should_persist_full_content(imgtype) {
        parse_img_response(content_type, bytes, ImageType::Content(None))?
    } else {
        display_img.clone()
    };

    Ok(DisplayAndDiskImages {
        display: display_img,
        disk: disk_img,
    })
}

/// Builds display and disk output for an image fetched from the network.
///
/// With a `secret`, `bytes` are the ciphertext of an encrypted blob: they
/// are decrypted and parsed as the secret's `m`, whatever content type the
/// server sent. A blob that fails to decrypt is an error, never drawn.
fn parse_fetched_image(
    content_type: Option<&str>,
    bytes: &[u8],
    secret: Option<&EncryptedImeta<'_>>,
    imgtype: ImageType,
) -> Result<DisplayAndDiskImages, crate::Error> {
    let Some(secret) = secret else {
        return parse_display_and_disk_images(content_type, bytes, imgtype);
    };

    let plaintext = decrypt(bytes, &secret.key, &secret.nonce)
        .map_err(|e| crate::Error::Generic(format!("decrypting {}: {e}", secret.url)))?;
    parse_display_and_disk_images(Some(&secret.mime), &plaintext, imgtype)
}

/// Loads a cached static image, resizing only when the stored image exceeds the requested [`ImageType`].
pub fn fetch_static_img_from_disk(
    ctx: egui::Context,
    url: &str,
    request_key: &TextureRequestKey,
    img_type: ImageType,
    path: &Path,
) -> Result<egui::TextureHandle, crate::Error> {
    tracing::trace!("Starting job static img from disk for {url}");
    let data = std::fs::read(path)?;
    let img = match image::load_from_memory(&data).map_err(crate::Error::Image) {
        Ok(image_buffer) => {
            let width = image_buffer.width();
            let height = image_buffer.height();
            if needs_resize(img_type, width, height) {
                process_image(img_type, image_buffer)
            } else {
                buffer_to_color_image(image_buffer.as_flat_samples_u8(), width, height)
            }
        }
        Err(image_err) => {
            tracing::debug!("raw cache decode failed, trying svg fallback: {image_err}");
            // SVGs are cached as source bytes and rasterized per-request.
            parse_img_response(Some("image/svg+xml"), &data, img_type)?
        }
    };

    Ok(load_texture_checked(
        &ctx,
        request_key.to_job_id(),
        img,
        Default::default(),
    ))
}

async fn fetch_static_img_from_net(
    url: String,
    request_key: TextureRequestKey,
    ctx: egui::Context,
    path: PathBuf,
    imgtype: ImageType,
    secret: Option<EncryptedImeta<'static>>,
) -> JobOutput<MediaJobResult> {
    tracing::trace!("fetch static img from net: starting job. sending http request for {url}");
    let res = match http_req(&url).await {
        Ok(r) => r,
        Err(e) => {
            return JobOutput::complete(MediaJobResult::StaticImg(Err(crate::Error::Generic(
                format!("Http error: {e}"),
            ))));
        }
    };

    tracing::trace!("static img from net: parsing http request from {url}");
    JobOutput::Next(JobRun::Sync(Box::new(move || {
        let DisplayAndDiskImages {
            display: display_img,
            disk: disk_img,
        } = match parse_fetched_image(
            res.content_type.as_deref(),
            &res.bytes,
            secret.as_ref(),
            imgtype,
        ) {
            Ok(imgs) => imgs,
            Err(e) => {
                return JobOutput::Complete(CompleteResponse::new(MediaJobResult::StaticImg(Err(
                    e,
                ))));
            }
        };

        let display_texture_handle = load_texture_checked(
            &ctx,
            request_key.to_job_id(),
            display_img,
            Default::default(),
        );

        JobOutput::Complete(
            CompleteResponse::new(MediaJobResult::StaticImg(Ok(display_texture_handle)))
                .run_no_output(NoOutputRun::Sync(Box::new(move || {
                    tracing::trace!("static img from net: Saving output from {url}");
                    if let Err(e) = MediaCache::write(&path, &url, disk_img) {
                        tracing::error!("{e}");
                    }
                }))),
        )
    })))
}

fn needs_resize(img_type: ImageType, width: u32, height: u32) -> bool {
    match img_type {
        ImageType::Profile(size) => width > size || height > size,
        ImageType::Content(Some(dimensions)) => width > dimensions.x || height > dimensions.y,
        ImageType::Content(None) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::encrypted::encrypt;

    /// A `width` x `height` PNG, standing in for a pasted screenshot.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 40, 90, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode png");
        bytes.into_inner()
    }

    /// The encrypted fixture: a 5x3 PNG as a Blossom server would serve it,
    /// and the imeta that opens it.
    struct EncryptedPng {
        ciphertext: Vec<u8>,
        imeta: EncryptedImeta<'static>,
    }

    fn encrypted_png() -> EncryptedPng {
        let blob = encrypt(&png(5, 3)).expect("encrypt");
        let url = format!("https://blossom.example/{}", hex::encode(blob.x));
        let imeta = EncryptedImeta::new(url, "image/png", None, &blob);
        EncryptedPng {
            ciphertext: blob.ciphertext,
            imeta,
        }
    }

    #[test]
    fn an_encrypted_png_decrypts_and_decodes_to_its_size() {
        let EncryptedPng { ciphertext, imeta } = encrypted_png();

        // The server calls it octet-stream; the imeta's `m` wins.
        let imgs = parse_fetched_image(
            Some("application/octet-stream"),
            &ciphertext,
            Some(&imeta),
            ImageType::Content(None),
        )
        .expect("decrypt and parse");

        assert_eq!(imgs.display.size, [5, 3]);
        assert_eq!(imgs.disk.size, [5, 3]);
    }

    #[test]
    fn a_wrong_key_is_an_error_not_an_image() {
        let EncryptedPng {
            ciphertext,
            mut imeta,
        } = encrypted_png();
        imeta.key[0] ^= 1;

        let res = parse_fetched_image(
            Some("image/png"),
            &ciphertext,
            Some(&imeta),
            ImageType::Content(None),
        );
        assert!(res.is_err());
    }

    #[test]
    fn an_unregistered_ciphertext_does_not_parse() {
        let EncryptedPng { ciphertext, .. } = encrypted_png();
        let res = parse_fetched_image(
            Some("image/png"),
            &ciphertext,
            None,
            ImageType::Content(None),
        );
        assert!(res.is_err());
    }
}
