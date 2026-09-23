macro_rules! path_buf {
    ($base:expr_2021, $( $x:expr_2021 ), *) => {
        {
            let mut temp = $base;
            $(
                temp.push($x);
            )*
            temp
        }
    }
}
