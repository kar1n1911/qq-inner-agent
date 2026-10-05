mod affect_support;
use qq_inner_core::affect::{self,disposition,Disposition::*};
#[test] fn quadrants_and_circuit() {
 assert_eq!(disposition(-1.,-1.),Angry);assert_eq!(disposition(-1.,1.),Withdrawn);
 assert_eq!(disposition(1.,1.),Scrutinizing);assert_eq!(disposition(1.,-1.),Supportive);
 assert!(Angry.motivation()>1. && Withdrawn.motivation()<Scrutinizing.motivation());
 assert_eq!(Scrutinizing.length(),"long");assert_eq!(Angry.length(),"short");
 let s=affect_support::store();
 for _ in 0..3 {assert!(affect::burst_allowed(&s,"a","1").unwrap());affect::reserve_burst(&s,"a","1").unwrap();}
 assert!(!affect::burst_allowed(&s,"a","1").unwrap());assert!(affect::reserve_burst(&s,"a","1").is_err());
 assert!(affect::burst_allowed(&s,"b","1").unwrap());assert!(affect::burst_allowed(&s,"a","3").unwrap());
}
