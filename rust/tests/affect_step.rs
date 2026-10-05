use qq_inner_core::affect::bounded_step;
#[test] fn asymmetric_confidence_bounded() {
 assert_eq!(bounded_step(-1.,1.),-0.15);
 assert_eq!(bounded_step(1.,1.),0.1);
 assert_eq!(bounded_step(-1.,0.5),-0.1);
 for n in -100..=100 {assert!(bounded_step(n as f64/100.,0.7).abs()<=0.15);}
 assert_eq!(bounded_step(1.,0.),0.);
}
