use aes::Aes128;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use cbc::{
    Encryptor,
    cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7},
};
use num_bigint::BigUint;
use rand::{Rng, rngs::OsRng};
use serde::Serialize;

const BASE62: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const IV: &[u8; 16] = b"0102030405060708";
const PRESET_KEY: &[u8; 16] = b"0CoJUm6Qyw8W8jud";
const PUBLIC_EXPONENT: u32 = 65_537;
const PUBLIC_MODULUS_HEX: &[u8] = b"e0b509f6259df8642dbc35662901477df22677ec152b5ff68ace615bb7b725152b3ab17a876aea8a5aa76d2e417629ec4ee341f56135fccf695280104e0312ecbda92557c93870114af6c9d05c4f7f0c3685b7a46bee255932575cce10b424d813cfe4875d3e82047b97ddef52741d546b8e289dc6935b3ece0462db0a22b8e7";

pub(super) struct Form {
    pub params: String,
    pub enc_sec_key: String,
}

pub(super) fn encrypt(payload: &impl Serialize) -> Result<Form, String> {
    let mut random = OsRng;
    let secret = std::array::from_fn(|_| BASE62[random.gen_range(0..BASE62.len())]);
    encrypt_with_secret(payload, &secret)
}

fn encrypt_with_secret(payload: &impl Serialize, secret: &[u8; 16]) -> Result<Form, String> {
    let json = serde_json::to_vec(payload).map_err(|error| error.to_string())?;
    let first = aes_cbc_base64(&json, PRESET_KEY)?;
    let params = aes_cbc_base64(first.as_bytes(), secret)?;
    let modulus = BigUint::parse_bytes(PUBLIC_MODULUS_HEX, 16)
        .ok_or_else(|| "invalid WEAPI RSA modulus".to_owned())?;
    let mut reversed_secret = *secret;
    reversed_secret.reverse();
    let message = BigUint::from_bytes_be(&reversed_secret);
    let encrypted = message.modpow(&BigUint::from(PUBLIC_EXPONENT), &modulus);
    let enc_sec_key = format!("{encrypted:0>256x}");
    Ok(Form {
        params,
        enc_sec_key,
    })
}

fn aes_cbc_base64(plaintext: &[u8], key: &[u8; 16]) -> Result<String, String> {
    let cipher = Encryptor::<Aes128>::new_from_slices(key, IV)
        .map_err(|error| format!("cannot initialize WEAPI AES: {error}"))?;
    Ok(BASE64.encode(cipher.encrypt_padded_vec_mut::<Pkcs7>(plaintext)))
}

#[cfg(test)]
mod tests {
    use super::{BASE64, encrypt_with_secret};
    use base64::Engine as _;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Payload<'a> {
        id: &'a str,
        br: &'a str,
    }

    #[test]
    fn encryption_has_stable_protocol_shaped_output_for_a_fixed_secret() {
        let form = encrypt_with_secret(
            &Payload {
                id: "301448",
                br: "874663",
            },
            b"abcdefghijklmnop",
        )
        .unwrap();

        assert_eq!(
            form.params,
            "O+9DbBO5GG5bZjS60Gmr1/sLGMec3aO+G9R6MjF9IA4tXMzITZ46DwEtDnLqOlgD"
        );
        assert_eq!(
            form.enc_sec_key,
            "d15a1683c992095d0c234c19966605c5c5964911268bbeda8cb8d08d834913e59d53b32358903a121b5fca784c1f5ae44951fd02524df58ecc98e52cc7cf8689b42c2e93ddf05b0592512d87f5960467e2f086c018849d76014d323500e30f13ef4cafbb0cf5a66731a3f1776c75ca35d0062dac70a3e33245afabcf47938487"
        );
        assert_eq!(BASE64.decode(&form.params).unwrap().len() % 16, 0);
        assert!(!form.params.contains("301448"));
        assert!(!form.params.contains("874663"));
    }
}
