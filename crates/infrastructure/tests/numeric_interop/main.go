package main

import (
	"encoding/hex"
	"encoding/json"
	"fmt"
	"github.com/hashicorp/terraform-plugin-go/tftypes"
	"github.com/vmihailenco/msgpack/v5"
	"github.com/zclconf/go-cty/cty"
	ctyjson "github.com/zclconf/go-cty/cty/json"
	ctymsgpack "github.com/zclconf/go-cty/cty/msgpack"
	"math"
	"math/big"
	"os"
)

type numberCase struct {
	MessagePack string          `json:"messagePack"`
	State       json.RawMessage `json:"state"`
	Literal     string          `json:"literal"`
	FloatBits   *uint64         `json:"floatBits"`
}

func main() {
	var cases []numberCase
	if json.NewDecoder(os.Stdin).Decode(&cases) != nil {
		panic("invalid numeric fixture")
	}
	var decoded []*big.Float
	var rationals []*big.Rat
	for _, item := range cases {
		expected, _, err := big.ParseFloat(item.Literal, 10, 512, big.ToNearestEven)
		if err != nil || expected.IsInf() {
			panic("invalid expected numeric value")
		}
		if item.FloatBits != nil {
			expected = new(big.Float).SetPrec(512).SetFloat64(math.Float64frombits(*item.FloatBits))
		}
		var actual6 big.Float
		actual6.Copy(expected)
		if item.MessagePack != "" {
			bytes, err := hex.DecodeString(item.MessagePack)
			if err != nil {
				panic("invalid wire fixture")
			}
			value6, err := tftypes.ValueFromMsgPack(bytes, tftypes.Number)
			if err != nil {
				panic("protocol 6 MessagePack decode failed")
			}
			if value6.As(&actual6) != nil || actual6.IsInf() || actual6.Cmp(expected) != 0 {
				panic("protocol 6 numeric value changed")
			}
			encoded6, err := value6.MarshalMsgPack(tftypes.Number)
			if err != nil {
				panic("protocol 6 numeric re-encoding failed")
			}
			returned6, err := tftypes.ValueFromMsgPack(encoded6, tftypes.Number)
			if err != nil {
				panic("protocol 6 numeric second decode failed")
			}
			var again6 big.Float
			if returned6.As(&again6) != nil || again6.Cmp(expected) != 0 {
				panic("protocol 6 re-encoding changed numeric value")
			}
			value5, err := ctymsgpack.Unmarshal(bytes, cty.Number)
			if err != nil || value5.AsBigFloat().Cmp(expected) != 0 {
				panic("protocol 5 numeric value changed")
			}
			encoded5, err := ctymsgpack.Marshal(value5, cty.Number)
			if err != nil {
				panic("protocol 5 numeric re-encoding failed")
			}
			returned5, err := ctymsgpack.Unmarshal(encoded5, cty.Number)
			if err != nil || returned5.AsBigFloat().Cmp(expected) != 0 {
				panic("protocol 5 re-encoding changed numeric value")
			}
		}
		state6, err := tftypes.ValueFromJSON(item.State, tftypes.Number)
		if err != nil {
			panic("protocol 6 numeric state rejected")
		}
		var stateNumber6 big.Float
		if state6.As(&stateNumber6) != nil || stateNumber6.Cmp(expected) != 0 {
			panic("protocol 6 numeric state changed")
		}
		state5, err := ctyjson.Unmarshal(item.State, cty.Number)
		if err != nil || state5.AsBigFloat().Cmp(expected) != 0 {
			panic("protocol 5 numeric state changed")
		}
		rational, ok := new(big.Rat).SetString(item.Literal)
		if !ok {
			panic("invalid exact rational fixture")
		}
		for index, earlier := range decoded {
			if rationals[index].Cmp(rational) != 0 && earlier.Cmp(&actual6) == 0 {
				panic("distinct admitted numeric values aliased")
			}
		}
		decoded = append(decoded, new(big.Float).Copy(&actual6))
		rationals = append(rationals, rational)
	}
	// A negative control demonstrates why huge integral native F64 is refused.
	wire, err := msgpack.Marshal(math.MaxFloat64)
	if err != nil {
		panic("MAX control encoding failed")
	}
	original, err := tftypes.ValueFromMsgPack(wire, tftypes.Number)
	if err != nil {
		panic("MAX control decoding failed")
	}
	againWire, err := original.MarshalMsgPack(tftypes.Number)
	if err != nil {
		panic("MAX control re-encoding failed")
	}
	again, err := tftypes.ValueFromMsgPack(againWire, tftypes.Number)
	if err != nil {
		panic("MAX second decoding failed")
	}
	var firstNumber, secondNumber big.Float
	if original.As(&firstNumber) != nil || again.As(&secondNumber) != nil || firstNumber.Cmp(&secondNumber) == 0 {
		panic("upstream integral F64 boundary changed")
	}
	fmt.Printf("%d numeric cases passed both protocol codecs and typed JSON state\n", len(cases))
}
