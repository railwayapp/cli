echo "hello"
env | grep RAILWAY

i=1
while true; do
  echo $i
  ((i++))
  sleep 2
done
