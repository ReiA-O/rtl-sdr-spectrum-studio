//! Reusable receive-path conditioning, independent of USB and application state.
//!
//! Center rejection changes the actual complex samples, not their display bins.
//! A real signal close to receiver DC is also attenuated. Callers must preserve
//! original IQ separately and identify this processed path in their metadata.
use rustfft::num_complex::Complex32;

/// Nominal one-pole corner in complex-baseband Hz, relative to receiver center.
pub const CENTER_DC_CUTOFF_HZ:f64=20.0;

/// Independent I/Q DC tracking and subtraction with f64 state.
///
/// The first nonempty block containing finite samples supplies the initial
/// complex mean. This suppresses a constant input from the first FFT rather than
/// leaving a start-up exponential in Max Hold. That initial seed intentionally
/// depends on the first block's samples/length; after seeding, the recurrence is
/// continuous and independent of later block boundaries. Reset on source changes
/// or discontinuities, not on each API block.
///
/// With pole `a = exp(-2*pi*20/fs)`, the steady-state transfer is
/// `(1+a)/2 * (1-z^-1)/(1-a*z^-1)`. Nyquist gain is unity. The corner is nominal
/// at finite fs, and converges to 20 Hz when fs is much larger than the corner.
#[derive(Clone,Debug)]
pub struct CenterDcRejector {
    smoothing:f64,
    output_scale:f64,
    mean_i:f64,
    mean_q:f64,
    seeded:bool,
}

impl CenterDcRejector {
    /// Construct for a validated sample rate. Use `try_new` for external input.
    pub fn new(sample_rate:f64)->Self {
        Self::try_new(sample_rate).expect("CenterDcRejector sample rate must be validated")
    }
    pub fn try_new(sample_rate:f64)->Result<Self,String> {
        if !sample_rate.is_finite() || sample_rate<=2.0*CENTER_DC_CUTOFF_HZ {
            return Err("中心DC補正には40 Hzを超える有限サンプルレートが必要です".into());
        }
        let exponent=-std::f64::consts::TAU*CENTER_DC_CUTOFF_HZ/sample_rate;
        let pole=exponent.exp();
        Ok(Self {smoothing:-exponent.exp_m1(),output_scale:(1.0+pole)/(2.0*pole),
            mean_i:0.0,mean_q:0.0,seeded:false})
    }
    /// Drop previous-source DC state and use a validated new sample rate.
    pub fn reset(&mut self,sample_rate:f64) {*self=Self::new(sample_rate);}

    pub fn process_in_place(&mut self,values:&mut [Complex32]) {
        if values.is_empty(){return;}
        if !self.seeded {
            let (mut sum_i,mut sum_q,mut count)=(0.0,0.0,0usize);
            for value in values.iter().filter(|v|v.re.is_finite()&&v.im.is_finite()) {
                sum_i+=value.re as f64;sum_q+=value.im as f64;count+=1;
            }
            if count==0 {values.fill(Complex32::new(0.0,0.0));return;}
            self.mean_i=sum_i/count as f64;self.mean_q=sum_q/count as f64;
            self.seeded=true;
        }
        for value in values {
            if !value.re.is_finite()||!value.im.is_finite() {
                // An invalid sample must not contaminate following valid IQ.
                *value=Complex32::new(0.0,0.0);continue;
            }
            let (i,q)=(value.re as f64,value.im as f64);
            self.mean_i+=self.smoothing*(i-self.mean_i);
            self.mean_q+=self.smoothing*(q-self.mean_q);
            // Real USB input is bounded. Clamp only floating-point overflow for
            // external callers supplying finite values near f32 extrema.
            *value=Complex32::new(to_f32((i-self.mean_i)*self.output_scale),
                to_f32((q-self.mean_q)*self.output_scale));
        }
    }
}

fn to_f32(value:f64)->f32 {value.clamp(f32::MIN as f64,f32::MAX as f64)as f32}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(fs:f64,hz:f64,samples:usize,amplitude:f64,dc:Complex32)->Vec<Complex32> {
        (0..samples).map(|n|{let phase=std::f64::consts::TAU*hz*n as f64/fs;
            dc+Complex32::new((amplitude*phase.cos())as f32,(amplitude*phase.sin())as f32)}).collect()
    }
    fn mean(values:&[Complex32])->(f64,f64) {
        let (i,q)=values.iter().fold((0.0,0.0),|(i,q),v|(i+v.re as f64,q+v.im as f64));
        (i/values.len()as f64,q/values.len()as f64)
    }
    fn amplitude(values:&[Complex32],fs:f64,hz:f64,start:usize)->f64 {
        let (i,q)=values.iter().enumerate().fold((0.0,0.0),|(i,q),(n,v)|{
            let phase=std::f64::consts::TAU*hz*(start+n)as f64/fs;
            (i+v.re as f64*phase.cos()+v.im as f64*phase.sin(),
             q+v.im as f64*phase.cos()-v.re as f64*phase.sin())});
        i.hypot(q)/values.len()as f64
    }

    #[test]
    fn constant_complex_dc_is_removed_from_the_first_block() {
        let mut values=vec![Complex32::new(-0.00103,-0.00094);131072];
        CenterDcRejector::new(2048000.0).process_in_place(&mut values);
        assert!(values.iter().all(|v|v.norm_sqr()<1e-24));
    }

    #[test]
    fn first_block_dc_is_suppressed_while_an_offcenter_carrier_survives() {
        let fs=2048000.0;let frequency=20000.0;let level=0.1;
        let mut values=tone(fs,frequency,131072,level,Complex32::new(-0.00103,-0.00103));
        CenterDcRejector::new(fs).process_in_place(&mut values);
        let (i,q)=mean(&values);
        assert!(i.hypot(q)<0.00002,"residual block mean={}",i.hypot(q));
        let (i,q)=mean(&values[..4096]);
        assert!(i.hypot(q)<0.00015,"initial FFT mean={}",i.hypot(q));
        assert!((amplitude(&values,fs,frequency,0)/level-1.0).abs()<0.00002);
    }

    #[test]
    fn initialized_stream_does_not_depend_on_later_block_boundaries() {
        let fs=2048000.0;let mut first=tone(fs,12345.0,4096,0.13,Complex32::new(0.007,-0.011));
        let mut single=CenterDcRejector::new(fs);single.process_in_place(&mut first);
        let mut split=single.clone();
        let mut a=tone(fs,23456.0,17003,0.11,Complex32::new(-0.02,0.03));let mut b=a.clone();
        single.process_in_place(&mut a);
        let mut at=0;
        for size in [1,73,4097,3,1000,8191,3638] {
            split.process_in_place(&mut b[at..at+size]);at+=size;
        }
        assert_eq!(at,b.len());assert_eq!(a,b);
    }

    #[test]
    fn reset_forgets_prior_offset_and_recomputes_rate() {
        let mut filter=CenterDcRejector::new(2048000.0);
        let mut old=vec![Complex32::new(0.4,-0.2);1024];filter.process_in_place(&mut old);
        filter.reset(2400000.0);
        let mut reset=tone(2400000.0,10000.0,16384,0.1,Complex32::new(-0.01,0.02));
        let mut fresh=reset.clone();filter.process_in_place(&mut reset);
        CenterDcRejector::new(2400000.0).process_in_place(&mut fresh);
        assert_eq!(reset,fresh);
    }

    #[test]
    fn physical_corner_is_recomputed_from_sample_rate() {
        for fs in [2000.0,4800.0] {
            let hz=20.0;let length=(fs*2.0)as usize;
            let mut values=tone(fs,hz,length,0.2,Complex32::new(0.01,-0.02));
            CenterDcRejector::new(fs).process_in_place(&mut values);
            let start=fs as usize;let measured=amplitude(&values[start..],fs,hz,start)/0.2;
            let pole=(-std::f64::consts::TAU*CENTER_DC_CUTOFF_HZ/fs).exp();
            let w=std::f64::consts::TAU*hz/fs;
            let expected=(1.0+pole)*0.5*(2.0-2.0*w.cos()).sqrt()
                /(1.0+pole*pole-2.0*pole*w.cos()).sqrt();
            assert!((measured-expected).abs()<0.000002,"fs={fs}: {measured} vs {expected}");
            assert!((measured-std::f64::consts::FRAC_1_SQRT_2).abs()<0.001);
        }
    }

    #[test]
    fn empty_invalid_and_extreme_input_cannot_poison_following_samples() {
        let mut filter=CenterDcRejector::new(2048000.0);
        filter.process_in_place(&mut []);assert!(!filter.seeded);
        let mut invalid=[Complex32::new(f32::NAN,0.0),Complex32::new(0.0,f32::INFINITY)];
        filter.process_in_place(&mut invalid);assert!(!filter.seeded);
        assert!(invalid.iter().all(|v|v.re==0.0&&v.im==0.0));
        let mut values=[Complex32::new(0.1,-0.1),Complex32::new(f32::INFINITY,0.0),
            Complex32::new(f32::MAX,-f32::MAX),Complex32::new(-f32::MAX,f32::MAX),Complex32::new(0.1,-0.1)];
        filter.process_in_place(&mut values);
        assert!(values.iter().all(|v|v.re.is_finite()&&v.im.is_finite()));
        let mut next=[Complex32::new(0.01,-0.02);1024];filter.process_in_place(&mut next);
        assert!(next.iter().all(|v|v.re.is_finite()&&v.im.is_finite()));
        for fs in [0.0,40.0,-1.0,f64::NAN,f64::INFINITY] {assert!(CenterDcRejector::try_new(fs).is_err());}
    }
}
