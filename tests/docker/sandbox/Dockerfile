FROM sysg-usecase-base
RUN mkdir -p /allowed /secret && echo ok > /allowed/ok.txt && echo topsecret > /secret/deny.txt
COPY tests/docker/sandbox/stack.yaml /etc/systemg/systemg.yaml
COPY tests/docker/sandbox/run_sandbox_tests.sh /run.sh
RUN chmod +x /run.sh
ENTRYPOINT ["/run.sh"]
